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

pub(crate) async fn serve_frontend_mounts(mounts: Arc<Vec<FrontendMount>>, req: Request) -> Option<Response> {
    if !matches!(
        *req.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    ) {
        return None;
    }

    let (mount, relative_path) = frontend_match(&mounts, req.uri().path())?;
    let file_path = frontend_safe_path(&mount.directory, &relative_path)?;
    if tokio::fs::metadata(&file_path)
        .await
        .is_ok_and(|metadata| metadata.is_file())
    {
        return serve_frontend_file(req, file_path, StatusCode::OK).await;
    }

    let fallback = mount.fallback.as_deref()?;
    let navigation = frontend_navigation_request(&req, &relative_path);
    let (fallback_path, status) = frontend_fallback_path(mount, fallback, navigation).await?;
    serve_frontend_file(req, fallback_path, status).await
}

pub(crate) async fn serve_frontend_file(req: Request, path: PathBuf, status: StatusCode) -> Option<Response> {
    let mut response = ServeFile::new(path)
        .oneshot(req)
        .await
        .ok()
        .map(IntoResponse::into_response)?;
    *response.status_mut() = status;
    Some(response)
}

pub(crate) fn add_static_mount(app: Router, mount: StaticMount) -> Router {
    let mount_path = if mount.path == "/" {
        "/".to_string()
    } else {
        mount.path.trim_end_matches('/').to_string()
    };

    let serve_dir = ServeDir::new(&mount.directory).append_index_html_on_directories(mount.html);
    if mount.follow_symlink {
        return app.nest_service(&mount_path, serve_dir);
    }

    let directory = Arc::new(PathBuf::from(mount.directory));
    let html = mount.html;
    let service = service_fn(move |req: Request| {
        let directory = directory.clone();
        let serve_dir = serve_dir.clone();
        async move {
            if static_request_hits_symlink(&directory, req.uri().path(), html).await {
                return Ok::<_, std::convert::Infallible>(StatusCode::FORBIDDEN.into_response());
            }

            serve_dir
                .oneshot(req)
                .await
                .map(IntoResponse::into_response)
        }
    });

    app.nest_service(&mount_path, service)
}

pub(crate) fn frontend_safe_path(directory: &str, request_path: &str) -> Option<PathBuf> {
    let decoded = percent_encoding::percent_decode_str(request_path)
        .decode_utf8()
        .ok()?;
    let mut file_path = PathBuf::from(directory);

    for component in Path::new(decoded.trim_start_matches('/')).components() {
        match component {
            Component::Normal(part) => file_path.push(part),
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => return None,
        }
    }

    if request_path.is_empty() || request_path.ends_with('/') {
        file_path.push("index.html");
    }

    Some(file_path)
}

pub(crate) fn frontend_navigation_request(req: &Request, relative_path: &str) -> bool {
    relative_path
        .rsplit('/')
        .next()
        .is_none_or(|last| !last.contains('.'))
        && req
            .headers()
            .get(axum::http::header::ACCEPT)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|accept| accept.contains("text/html") || accept.contains("*/*"))
}

pub(crate) async fn frontend_fallback_path(
    mount: &FrontendMount,
    fallback: &str,
    navigation: bool,
) -> Option<(PathBuf, StatusCode)> {
    let directory = Path::new(&mount.directory);
    let candidates: SmallVec<[(PathBuf, StatusCode); 2]> = if fallback == "auto" {
        smallvec::smallvec![
            (directory.join("404.html"), StatusCode::NOT_FOUND),
            (directory.join("index.html"), StatusCode::OK),
        ]
    } else if fallback == "404.html" {
        smallvec::smallvec![(directory.join(fallback), StatusCode::NOT_FOUND)]
    } else if navigation {
        smallvec::smallvec![(directory.join(fallback), StatusCode::OK)]
    } else {
        return None;
    };

    for (path, status) in candidates {
        if status == StatusCode::OK && !navigation {
            continue;
        }
        if tokio::fs::metadata(&path)
            .await
            .is_ok_and(|metadata| metadata.is_file())
        {
            return Some((path, status));
        }
    }

    None
}


pub(crate) async fn record_prometheus_metrics(req: Request, next: Next) -> Response {
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    let start = Instant::now();
    let response = next.run(req).await;
    let status = response.status().as_u16().to_string();
    let elapsed = start.elapsed().as_secs_f64();

    metrics::counter!(
        "fastrapi_requests_total",
        "method" => method.clone(),
        "path" => path.clone(),
        "status" => status.clone(),
    )
    .increment(1);
    metrics::histogram!(
        "fastrapi_request_duration_seconds",
        "method" => method,
        "path" => path,
        "status" => status,
    )
    .record(elapsed);

    response
}
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

pub(crate) fn frontend_match<'a>(
    mounts: &'a [FrontendMount],
    request_path: &str,
) -> Option<(&'a FrontendMount, String)> {
    mounts
        .iter()
        .filter_map(|mount| {
            if mount.path == "/" {
                return Some((mount, request_path.trim_start_matches('/').to_string()));
            }
            if request_path == mount.path {
                return Some((mount, String::new()));
            }
            request_path
                .strip_prefix(&format!("{}/", mount.path))
                .map(|relative| (mount, relative.to_string()))
        })
        .max_by_key(|(mount, _)| mount.path.len())
}

pub(crate) async fn static_request_hits_symlink(directory: &Path, request_path: &str, html: bool) -> bool {
    let decoded = match percent_encoding::percent_decode_str(request_path).decode_utf8() {
        Ok(decoded) => decoded,
        Err(_) => return false,
    };
    if tokio::fs::symlink_metadata(directory)
        .await
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return true;
    }

    let mut file_path = directory.to_path_buf();

    for component in Path::new(decoded.trim_start_matches('/')).components() {
        match component {
            Component::Normal(part) => file_path.push(part),
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => return false,
        }

        if tokio::fs::symlink_metadata(&file_path)
            .await
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return true;
        }
    }

    if html
        && let Ok(metadata) = tokio::fs::metadata(&file_path).await
        && metadata.is_dir()
    {
        file_path.push("index.html");
        return tokio::fs::symlink_metadata(&file_path)
            .await
            .is_ok_and(|m| m.file_type().is_symlink());
    }

    false
}
