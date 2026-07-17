use crate::engine::server::*;
use crate::engine::types::{FastrAPI, FrontendMount, StaticMount};
use ahash::{AHashMap, AHashSet};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{ConnectInfo, Request},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header::CONTENT_TYPE},
    middleware::{self as axum_middleware, Next},
    response::{Html, IntoResponse, Response},
    routing::{MethodRouter, *},
    serve::ListenerExt,
};
use dashmap::DashMap;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use notify::{Config as NotifyConfig, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use pyo3::{
    exceptions::{PyRuntimeError, PyTypeError},
    intern,
    prelude::*,
};
use smallvec::SmallVec;
use std::{
    collections::hash_map::Entry,
    net::{IpAddr, SocketAddr},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use tokio::net::TcpListener;
use tower::{ServiceExt, service_fn};
use tower_http::{
    catch_panic::CatchPanicLayer,
    compression::{CompressionLayer, predicate::SizeAbove},
    normalize_path::NormalizePathLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use tower_sessions::{Expiry, MemoryStore, SessionManagerLayer, cookie::Key};
use tracing::{Level, error, info};

use crate::{
    ffi::py_handlers::{
        ExecutionMode, render_no_request_json_response, render_no_request_response, run_py_handler,
        run_py_handler_no_request,
    },
    globals::{MIDDLEWARES, PYTHON_RUNTIME},
    http::{
        middleware::{
            CORSMiddleware, GZipMiddleware, HTTPSRedirectMiddleware, SessionMiddleware,
            TrustedHostMiddleware, build_cors_layer, parse_cors_params, parse_gzip_params,
            parse_https_redirect_params, parse_session_params, parse_trusted_host_params,
        },
        websocket::ws_handler,
    },
    routing::{
        prometheus::prometheus_handle,
        router::{FrozenRouter, FrozenRouterBuilder, RouteMatch},
        types::{BodyField, BodyPayload, HttpMethod, PathParamRange, RouteHandler, UploadedFile},
    },
    utils::{local_guard, openapi::build_openapi_spec, py_any_to_json},
};

#[derive(Clone, Hash, PartialEq, Eq)]
pub(crate) struct RateLimitKey {
    handler: usize,
    ip: Option<IpAddr>,
}

pub(crate) struct RateLimitWindow {
    start: Instant,
    count: u32,
}

pub(crate) static RATE_LIMITS: OnceLock<DashMap<RateLimitKey, Mutex<RateLimitWindow>>> = OnceLock::new();

pub(crate) fn is_rate_limited(req: &Request, handler: usize, limit: u32) -> bool {
    if limit == 0 {
        return true;
    }

    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let key = RateLimitKey { handler, ip };
    let limits = RATE_LIMITS.get_or_init(DashMap::new);
    let bucket = limits.entry(key).or_insert_with(|| {
        Mutex::new(RateLimitWindow {
            start: Instant::now(),
            count: 0,
        })
    });
    let mut window = bucket.lock();
    let now = Instant::now();

    if now.duration_since(window.start) >= Duration::from_secs(1) {
        window.start = now;
        window.count = 0;
    }

    if window.count >= limit {
        return true;
    }

    window.count += 1;
    false
}
