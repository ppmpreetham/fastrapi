use super::dispatch::*;
use super::files::*;
use super::lifecycle::*;
use super::serve::*;

use crate::engine::types::FastrAPI;
use ahash::{AHashMap, AHashSet};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::Request,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header::CONTENT_TYPE},
    middleware::{self as axum_middleware, Next},
    response::{Html, IntoResponse, Response},
    routing::{MethodRouter, *},
};
use pyo3::prelude::*;
use std::time::Instant;
use std::{sync::Arc, time::Duration};
use tower_http::{
    catch_panic::CatchPanicLayer,
    compression::{CompressionLayer, predicate::SizeAbove},
    normalize_path::NormalizePathLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    set_header::SetResponseHeaderLayer,
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use tower_sessions::{Expiry, MemoryStore, SessionManagerLayer, cookie::Key};
use tracing::info;

use crate::{
    ffi::py_handlers::{
        ExecutionMode, render_no_request_json_response, render_no_request_response,
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
        router::FrozenRouterBuilder,
        types::{HttpMethod, RouteHandler},
    },
    utils::{local_guard, openapi::build_openapi_spec, py_any_to_json},
};

macro_rules! match_method_router {
    ($method:expr, $handler:expr) => {
        match $method {
            HttpMethod::GET => get($handler),
            HttpMethod::POST => post($handler),
            HttpMethod::PUT => put($handler),
            HttpMethod::DELETE => delete($handler),
            HttpMethod::PATCH => patch($handler),
            HttpMethod::OPTIONS => options($handler),
            HttpMethod::HEAD => head($handler),
        }
    };
}

pub(crate) fn build_router(
    py: Python,
    app_state: AppState,
    docs_url: Option<String>,
    openapi_url: String,
    app_config: &FastrAPI,
) -> Router {
    let mut app = Router::new();

    let mut session_config = app_config.session_config.clone();
    let mut gzip_config = app_config.gzip_config.clone();
    let mut cors_config = app_config.cors_config.clone();
    let mut trusted_host_config = app_config.trusted_host_config.clone();
    let mut https_redirect_config = app_config.https_redirect_config.clone();

    merge_declared_middlewares(
        py,
        app_config,
        &mut cors_config,
        &mut trusted_host_config,
        &mut https_redirect_config,
        &mut gzip_config,
        &mut session_config,
    );

    let base_router = app_config.router.bind(py);
    let base_ref = base_router.borrow();
    base_ref.freeze(py);
    let flat = base_ref.flatten(py);

    let mut frozen_router_builder = FrozenRouterBuilder::new();
    flat.0.iter().for_each(|route| {
        frozen_router_builder.add_route(route.method, route.path.clone(), route.handler.clone());
    });

    let frozen_router = Arc::new(frozen_router_builder.build());
    let frontend_mounts = Arc::new(app_config.frontend_mounts.clone());

    let mut cached_routes: AHashMap<String, MethodRouter> = AHashMap::new();
    flat.0
        .iter()
        .filter(|route| {
            route.handler.cache_response
                && !route.path.contains('{')
                && matches!(route.handler.execution_mode, ExecutionMode::SyncNoArgs)
        })
        .filter_map(|route| {
            precompute_const_response(py, &route.handler)
                .map(|cached| (route.path.clone(), route.method, cached))
        })
        .for_each(|(path, method, cached)| {
            let method_router = cached_method_router(method, cached);
            cached_routes
                .entry(path)
                .and_modify(|existing| *existing = existing.clone().merge(method_router.clone()))
                .or_insert(method_router);
        });

    app = cached_routes
        .into_iter()
        .fold(app, |current_app, (path, method_router)| {
            current_app.route(&path, method_router)
        });

    let mut direct_no_request_routes: AHashMap<String, MethodRouter> = AHashMap::new();
    flat.0
        .iter()
        .filter(|route| {
            !route.handler.cache_response
                && !route.path.contains('{')
                && matches!(
                    route.handler.execution_mode,
                    ExecutionMode::SyncNoArgs | ExecutionMode::AsyncNoArgs
                )
        })
        .for_each(|route| {
            let method_router =
                no_request_method_router(route.method, route.handler.clone(), app_state.clone());
            direct_no_request_routes
                .entry(route.path.clone())
                .and_modify(|existing| *existing = existing.clone().merge(method_router.clone()))
                .or_insert(method_router);
        });

    app = direct_no_request_routes
        .into_iter()
        .fold(app, |current_app, (path, method_router)| {
            current_app.route(&path, method_router)
        });

    app = flat.1.iter().fold(app, |current_app, ws| {
        let path = ws.path.clone();
        let handler = Arc::new(ws.handler.clone_ref(py));
        let rt_handle = app_state.rt_handle.clone();
        let async_loop = app_state.async_loop.clone();

        current_app.route(
            &path,
            axum::routing::get(move |ws_upgrade| {
                ws_handler(
                    ws_upgrade,
                    axum::Extension(handler.clone()),
                    axum::Extension(rt_handle.clone()),
                    axum::Extension(async_loop.clone()),
                )
            }),
        )
    });

    app = app_config
        .static_mounts
        .iter()
        .cloned()
        .fold(app, |current_app, mount| {
            add_static_mount(current_app, mount)
        });

    let openapi_json = Arc::new(build_openapi_spec(py, app_config));

    app = app.route(
        &openapi_url,
        get({
            let json = openapi_json.clone();
            move || {
                let json = json.clone();
                async move { Json(json.as_ref().clone()) }
            }
        }),
    );
    if let Some(docs) = docs_url {
        let mut swagger_html = include_str!("../../../static/swagger-ui.html").to_string();
        if let Some(params) = &app_config.swagger_ui_parameters {
            let json_val = py_any_to_json(py, params.bind(py));
            if let Ok(json_str) = sonic_rs::to_string(&json_val) {
                swagger_html = swagger_html.replace("/* SWAGGER_UI_PARAMS */ {}", &json_str);
            }
        }
        let swagger_html = Arc::new(swagger_html);
        app = app.route(
            &docs,
            get({
                let html = swagger_html.clone();
                move || {
                    let html = html.clone();
                    async move { Html(html.as_ref().clone()) }
                }
            }),
        );
    }
    if let Some(redoc) = &app_config.redoc_url {
        app = app.route(
            redoc,
            get(|| async { Html(include_str!("../../../static/redoc.html")) }),
        );
    }
    if let Some(scalar) = &app_config.scalar_url {
        app = app.route(
            scalar,
            get(|| async { Html(include_str!("../../../static/scalar.html")) }),
        );
    }
    if let Some(elements) = &app_config.elements_url {
        app = app.route(
            elements,
            get(|| async { Html(include_str!("../../../static/elements.html")) }),
        );
    }

    if let Some(config) = &app_config.prometheus_config {
        let handle = prometheus_handle();
        app = app.route(
            &config.metrics_path,
            get(move || {
                let handle = handle.clone();
                async move { handle.render() }
            }),
        );
    }

    app = app.fallback({
        let router = frozen_router.clone();
        let state = app_state.clone();
        let frontend_mounts = frontend_mounts.clone();
        axum::routing::any(move |req: Request| async move {
            if request_matches_router(&router, &state, &req) {
                return dispatch(router, state, req).await;
            }

            serve_frontend_mounts(frontend_mounts, req)
                .await
                .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
        })
    });

    // L1: Sessions
    if let Some(config) = session_config {
        info!("???? Layer: Sessions");
        let key = Key::from(config.secret_key.as_bytes());
        let store = MemoryStore::default();

        let layer = SessionManagerLayer::new(store)
            .with_signed(key)
            .with_name(config.session_cookie.clone())
            .with_path(config.path.clone())
            .with_secure(config.https_only);

        let layer = if let Some(max_age) = config.max_age {
            layer.with_expiry(Expiry::OnInactivity(
                tower_sessions::cookie::time::Duration::seconds(max_age),
            ))
        } else {
            layer
        };

        app = app.layer(layer);
    }

    // L2: GZip
    if let Some(config) = gzip_config {
        info!("???????  Layer: GZip (min: {} bytes)", config.minimum_size);
        let predicate = SizeAbove::new(config.minimum_size as u16);
        app = app.layer(CompressionLayer::new().compress_when(predicate));
    }

    if app_config.prometheus_config.is_some() {
        app = app.layer(axum_middleware::from_fn(record_prometheus_metrics));
    }

    // L3: Python Middleware
    if !MIDDLEWARES.is_empty() {
        info!("Applying {} custom Python middleware(s)", MIDDLEWARES.len());

        let guard = local_guard(&*MIDDLEWARES);
        let middlewares = Arc::new(
            MIDDLEWARES
                .iter(&guard)
                .map(|(_key, middleware_ref)| middleware_ref.clone())
                .collect::<Vec<_>>(),
        );
        app =
            app.layer(axum_middleware::from_fn(move |req, next| {
                let middlewares = middlewares.clone();
                async move {
                    crate::http::middleware::execute_py_middlewares(middlewares, req, next).await
                }
            }));
    }

    // L4: HTTPS Redirect
    if let Some(_config) = https_redirect_config {
        info!("🔗 Layer: HTTPSRedirect");
        app = app.layer(axum_middleware::from_fn(
            move |req: Request, next: Next| async move {
                let uri = req.uri().clone();
                let headers = req.headers().clone();

                let mut is_https = false;
                if let Some(scheme) = uri.scheme() {
                    if scheme == &axum::http::uri::Scheme::HTTPS {
                        is_https = true;
                    }
                } else if let Some(forwarded_proto) = headers.get("X-Forwarded-Proto")
                    && forwarded_proto == "https"
                {
                    is_https = true;
                }

                if !is_https {
                    let mut parts = uri.into_parts();
                    parts.scheme = Some(axum::http::uri::Scheme::HTTPS);
                    if let Some(host) = headers.get("host").and_then(|h| h.to_str().ok()) {
                        parts.authority = Some(host.parse().unwrap());
                    }
                    if let Ok(new_uri) = axum::http::Uri::from_parts(parts) {
                        return (
                            StatusCode::TEMPORARY_REDIRECT,
                            [(axum::http::header::LOCATION, new_uri.to_string())],
                            "Redirecting...",
                        )
                            .into_response();
                    }
                }

                next.run(req).await
            },
        ));
    }

    // L5: Trusted Host
    if let Some(config) = trusted_host_config {
        info!("???? Layer: TrustedHost");
        let allow_all = config.allowed_hosts.iter().any(|host| host == "*");

        if !allow_all {
            let allowed: Arc<AHashSet<String>> =
                Arc::new(config.allowed_hosts.into_iter().collect());
            let redirect = config.www_redirect;

            app = app.layer(axum_middleware::from_fn(move |req: Request, next: Next| {
                let allowed = allowed.clone();
                async move {
                    let host_header = req
                        .headers()
                        .get("host")
                        .and_then(|h| h.to_str().ok())
                        .unwrap_or("")
                        .split(':')
                        .next()
                        .unwrap_or("");

                    if allowed.contains(host_header) {
                        return next.run(req).await;
                    }

                    if redirect && host_header.starts_with("www.") {
                        let root = host_header.strip_prefix("www.").unwrap();
                        if allowed.contains(root) {
                            return (StatusCode::MOVED_PERMANENTLY, "Redirecting...")
                                .into_response();
                        }
                    }

                    (StatusCode::BAD_REQUEST, "Invalid Host Header").into_response()
                }
            }));
        }
    }

    // L5: CORS. Apply last so preflight can terminate before other layers.
    if let Some(config) = cors_config {
        info!("Layer: CORS");
        match build_cors_layer(&config) {
            Ok(layer) => app = app.layer(layer),
            Err(e) => eprintln!("Error building CORS layer: {:?}", e),
        }
    }

    if app_config.trace_requests {
        app = app.layer(TraceLayer::new_for_http());
    }

    if let Some(header_name) = app_config
        .request_id_header
        .as_deref()
        .and_then(parse_header_name)
    {
        app = app
            .layer(SetRequestIdLayer::new(header_name.clone(), MakeRequestUuid))
            .layer(PropagateRequestIdLayer::new(header_name));
    }

    if let Some(value) = app_config
        .powered_by_header
        .as_deref()
        .and_then(parse_header_value)
    {
        app = app.layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("x-powered-by"),
            value,
        ));
    }

    if let Some(seconds) = app_config.request_timeout {
        app = app.layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(seconds),
        ));
    }

    if app_config.catch_panics {
        app = app.layer(CatchPanicLayer::new());
    }

    if app_config.redirect_slashes {
        app = app.layer(NormalizePathLayer::trim_trailing_slash());
    }

    app
}

pub(crate) fn merge_declared_middlewares(
    py: Python<'_>,
    app_config: &FastrAPI,
    cors_config: &mut Option<CORSMiddleware>,
    trusted_host_config: &mut Option<TrustedHostMiddleware>,
    https_redirect_config: &mut Option<HTTPSRedirectMiddleware>,
    gzip_config: &mut Option<GZipMiddleware>,
    session_config: &mut Option<SessionMiddleware>,
) {
    let Some(middlewares) = &app_config.middleware else {
        return;
    };

    let middlewares = middlewares.bind(py);
    let Ok(iter) = middlewares.try_iter() else {
        return;
    };

    iter.flatten().for_each(|item| {
        if let Err(err) = apply_declared_middleware(
            py,
            &item,
            cors_config,
            trusted_host_config,
            https_redirect_config,
            gzip_config,
            session_config,
        ) {
            log_python_error("middleware setup failed", err);
        }
    });
}

pub(crate) fn apply_declared_middleware(
    _py: Python<'_>,
    middleware_item: &Bound<'_, PyAny>,
    cors_config: &mut Option<CORSMiddleware>,
    trusted_host_config: &mut Option<TrustedHostMiddleware>,
    https_redirect_config: &mut Option<HTTPSRedirectMiddleware>,
    gzip_config: &mut Option<GZipMiddleware>,
    session_config: &mut Option<SessionMiddleware>,
) -> PyResult<()> {
    if let Ok(config) = middleware_item.extract::<PyRef<'_, CORSMiddleware>>() {
        *cors_config = Some(config.clone());
        return Ok(());
    }
    if let Ok(config) = middleware_item.extract::<PyRef<'_, TrustedHostMiddleware>>() {
        *trusted_host_config = Some(config.clone());
        return Ok(());
    }
    if let Ok(config) = middleware_item.extract::<PyRef<'_, HTTPSRedirectMiddleware>>() {
        *https_redirect_config = Some(config.clone());
        return Ok(());
    }
    if let Ok(config) = middleware_item.extract::<PyRef<'_, GZipMiddleware>>() {
        *gzip_config = Some(config.clone());
        return Ok(());
    }
    if let Ok(config) = middleware_item.extract::<PyRef<'_, SessionMiddleware>>() {
        *session_config = Some(config.clone());
        return Ok(());
    }

    let Ok(cls) = middleware_item.getattr("cls") else {
        return Ok(());
    };
    let Ok(kwargs_any) = middleware_item.getattr("kwargs") else {
        return Ok(());
    };
    let Ok(kwargs) = kwargs_any.cast::<pyo3::types::PyDict>() else {
        return Ok(());
    };
    let class_name_obj = cls.getattr("__name__")?;
    let class_name = class_name_obj
        .cast::<pyo3::types::PyString>()?
        .to_str()?
        .to_owned();

    match class_name.as_str() {
        "CORSMiddleware" => *cors_config = Some(parse_cors_params(kwargs)?),
        "TrustedHostMiddleware" => *trusted_host_config = Some(parse_trusted_host_params(kwargs)?),
        "HTTPSRedirectMiddleware" => {
            *https_redirect_config = Some(parse_https_redirect_params(kwargs)?)
        }
        "GZipMiddleware" => *gzip_config = Some(parse_gzip_params(kwargs)?),
        "SessionMiddleware" => *session_config = Some(parse_session_params(kwargs)?),
        _ => {}
    }

    Ok(())
}

pub(crate) fn cached_method_router(
    method: HttpMethod,
    cached: Arc<CachedResponse>,
) -> MethodRouter {
    match_method_router!(method, {
        let cached = cached.clone();
        move || {
            let cached = cached.clone();
            async move { cached.to_response() }
        }
    })
}

pub(crate) fn no_request_method_router(
    method: HttpMethod,
    handler: Arc<crate::routing::types::RouteHandler>,
    state: AppState,
) -> MethodRouter {
    if matches!(handler.execution_mode, ExecutionMode::SyncNoArgs) && !state.sync_to_threadpool {
        return sync_no_request_method_router(method, handler);
    }

    match_method_router!(method, {
        let handler = handler.clone();
        let state = state.clone();
        move || {
            let handler = handler.clone();
            let state = state.clone();
            async move {
                run_py_handler_no_request(
                    state.rt_handle,
                    state.async_loop,
                    state.sync_to_threadpool,
                    handler,
                )
                .await
            }
        }
    })
}

pub(crate) fn sync_no_request_method_router(
    method: HttpMethod,
    handler: Arc<crate::routing::types::RouteHandler>,
) -> MethodRouter {
    let use_json_fast_path = handler.response_model.is_none()
        && handler.response_class.is_none()
        && matches!(
            handler.response_type,
            crate::types::response::ResponseType::Json
        );

    match_method_router!(method, {
        let handler = handler.clone();
        move || {
            let handler = handler.clone();
            async move {
                Python::attach(|py| {
                    if use_json_fast_path {
                        render_no_request_json_response(py, &handler)
                    } else {
                        render_no_request_response(py, &handler)
                    }
                })
            }
        }
    })
}

pub(crate) fn precompute_const_response(
    py: Python<'_>,
    handler: &Arc<RouteHandler>,
) -> Option<Arc<CachedResponse>> {
    let response = render_no_request_response(py, handler);
    let status = response.status();
    let headers = cached_headers(response.headers());
    let body = PYTHON_RUNTIME
        .block_on(to_bytes(response.into_body(), usize::MAX))
        .ok()?;

    Some(Arc::new(CachedResponse {
        status,
        headers,
        body,
    }))
}

#[derive(Clone)]
pub(crate) struct CachedResponse {
    status: StatusCode,
    headers: CachedHeaders,
    body: bytes::Bytes,
}

#[derive(Clone)]
pub(crate) enum CachedHeaders {
    Empty,
    ContentType(HeaderValue),
    Full(HeaderMap),
}

impl CachedResponse {
    fn to_response(&self) -> Response {
        let mut response = Body::from(self.body.clone()).into_response();
        *response.status_mut() = self.status;
        match &self.headers {
            CachedHeaders::Empty => {}
            CachedHeaders::ContentType(content_type) => {
                response
                    .headers_mut()
                    .insert(CONTENT_TYPE, content_type.clone());
            }
            CachedHeaders::Full(headers) => {
                *response.headers_mut() = headers.clone();
            }
        }
        response
    }
}

pub(crate) fn cached_headers(headers: &HeaderMap) -> CachedHeaders {
    if headers.is_empty() {
        return CachedHeaders::Empty;
    }

    if headers.len() == 1
        && let Some(content_type) = headers.get(CONTENT_TYPE)
    {
        return CachedHeaders::ContentType(content_type.clone());
    }

    CachedHeaders::Full(headers.clone())
}

pub(crate) fn parse_header_name(value: &str) -> Option<HeaderName> {
    HeaderName::from_bytes(value.as_bytes()).ok()
}

pub(crate) fn parse_header_value(value: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(value).ok()
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
