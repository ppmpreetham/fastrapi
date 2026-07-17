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

pub(crate) async fn dispatch(router: Arc<FrozenRouter>, state: AppState, req: Request) -> Response {
    let Ok(method) = HttpMethod::try_from(req.method()) else {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    };

    let Some(path_str) = dispatch_path(&state, req.uri().path()) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let route_match = match router.resolve(method, path_str) {
        Some(v) => v,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    let (handler, params_iter) = match route_match {
        RouteMatch::Static(handler) => (handler, None),
        RouteMatch::Params(handler, params) => (handler, Some(params)),
    };

    if let Some(limit) = handler.rate_limit_per_second
        && is_rate_limited(&req, Arc::as_ptr(&handler) as usize, limit)
    {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }

    if matches!(
        handler.execution_mode,
        ExecutionMode::SyncNoArgs | ExecutionMode::AsyncNoArgs
    ) {
        return run_py_handler_no_request(
            state.rt_handle,
            state.async_loop,
            state.sync_to_threadpool,
            handler,
        )
        .await;
    }

    let path_base = path_str.as_ptr() as usize;
    let param_ranges: SmallVec<[PathParamRange; 4]> = if let Some(params) = params_iter {
        params
            .iter()
            .map(|(k, v)| {
                let start = v.as_ptr() as usize - path_base;
                debug_assert!(
                    start <= path_str.len(),
                    "matchit returned a string outside the input path"
                );
                PathParamRange {
                    key: k.to_string(),
                    start,
                    end: start + v.len(),
                }
            })
            .collect()
    } else {
        SmallVec::new()
    };

    let (request_parts, body) = req.into_parts();
    let has_body_requirements = !handler.body_param_indices.is_empty();

    let payload = if has_body_requirements {
        match extract_payload(&request_parts.headers, body, &handler, &state).await {
            Ok(p) => p,
            Err(resp) => return resp,
        }
    } else {
        None
    };

    run_py_handler(
        state.rt_handle,
        state.async_loop,
        state.sync_to_threadpool,
        handler,
        request_parts,
        param_ranges,
        payload,
    )
    .await
}

pub(crate) fn dispatch_path<'a>(state: &AppState, original_path: &'a str) -> Option<&'a str> {
    let root = state.root_path.trim_end_matches('/');
    if root.is_empty() {
        Some(original_path)
    } else if original_path == root {
        Some("/")
    } else if let Some(stripped) = original_path.strip_prefix(root) {
        stripped.starts_with('/').then_some(stripped)
    } else {
        None
    }
}
pub(crate) fn request_matches_router(router: &FrozenRouter, state: &AppState, req: &Request) -> bool {
    let Ok(method) = HttpMethod::try_from(req.method()) else {
        return true;
    };

    let Some(path) = dispatch_path(state, req.uri().path()) else {
        return false;
    };

    router.resolve(method, path).is_some()
}
