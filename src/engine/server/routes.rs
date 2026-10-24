//! The construction of the axum router from the routes of the app.
//! It registers the route handlers, the documentation endpoints and the cached responses.
//! It also contains the 404 and 405 responses.

use super::dispatch::dispatch_or_not_found;
use super::files::{add_static_mount, serve_frontend_mounts};
use super::lifecycle::log_python_error;
use super::middleware_stack;
use super::serve::AppState;

use crate::engine::config::FastrAPI;
use ahash::AHashMap;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::handler::Handler;
use axum::http::header::{ALLOW, CONTENT_TYPE, HOST};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{MethodFilter, MethodRouter, any, get, on};
use axum::{Json, Router};
use bytes::Bytes;
use pyo3::prelude::*;
use simd_json::OwnedValue as JsonValue;
use std::sync::Arc;
use std::time::Instant;

use crate::engine::exception_dispatch::dispatch_status_handler;
use crate::engine::executor::{
    render_no_request_json_response, render_no_request_response, run_py_handler_no_request,
};
use crate::engine::metrics::prometheus_handle;
use crate::engine::server::websocket::{WsRouteState, ws_handler, ws_param_names};
use fastrapi_core::method::HttpMethod;
use fastrapi_core::path::{join_path, strip_root_path};
use fastrapi_core::router::{FrozenRouter, FrozenRouterBuilder, RoutePattern};
use fastrapi_http::middleware::{MIDDLEWARE_REGISTRY, MiddlewareContainer, PyMiddleware};
use fastrapi_http::response_type::ResponseType;
use fastrapi_pyutil::json::py_any_to_json;
use fastrapi_routing::route::{ExecutionMode, RouteEntry, RouteHandler, WebSocketEntry};
use fastrapi_runtime::py_bridge;
use fastrapi_runtime::server_state::set_exception_handlers;
use fastrapi_runtime::tokio_runtime::PYTHON_RUNTIME;
use metrics::SharedString;
use pyo3::types::{PyDict, PyString};

pub(crate) fn build_router(
    py: Python,
    app_state: &'static AppState,
    docs_url: Option<&str>,
    openapi_url: Option<&str>,
    app_config: &FastrAPI,
) -> Router {
    let mut middlewares = app_config.middlewares.clone();
    merge_declared_middlewares(py, app_config, &mut middlewares);

    let base_router = app_config.router.bind(py);
    {
        let mut base_mut = base_router.borrow_mut();
        if base_mut.dependencies.is_none() {
            base_mut.dependencies.clone_from(&app_config.dependencies);
        }
    }
    if let Some(handlers) = &app_config.exception_handlers
        && let Ok(dict) = handlers.bind(py).cast::<PyDict>()
    {
        set_exception_handlers(dict.clone().unbind());
    }

    let base_ref = base_router.borrow();
    base_ref.freeze(py);
    let base_flat = base_ref.flatten(py);

    // Merge mounted sub-applications' routes under their prefixes.
    let mut routes = base_flat.0.clone();
    let mut ws_routes = base_flat.1.clone();
    for mount in &app_config.app_mounts {
        let sub = mount.app.bind(py);
        let sub_ref = sub.borrow();
        let sub_router = sub_ref.router.bind(py);
        {
            let mut sub_mut = sub_router.borrow_mut();
            if sub_mut.dependencies.is_none() {
                sub_mut.dependencies.clone_from(&sub_ref.dependencies);
            }
        }
        sub_router.borrow().freeze(py);
        let sub_flat = sub_router.borrow().flatten(py);

        routes.extend(sub_flat.0.iter().map(|route| {
            let mut route = route.clone();
            route.path = join_path(&mount.path, &route.path);
            route
        }));
        ws_routes.extend(sub_flat.1.iter().map(|ws| {
            let mut ws = ws.clone();
            ws.path = join_path(&mount.path, &ws.path);
            ws
        }));
    }

    // Host-mounted sub-apps: routes are served un-prefixed but only match
    // when the request's Host header equals the mount's host.
    for mount in &app_config.host_mounts {
        let sub = mount.app.bind(py);
        let sub_ref = sub.borrow();
        let sub_router = sub_ref.router.bind(py);
        sub_router.borrow().freeze(py);
        let sub_flat = sub_router.borrow().flatten(py);

        routes.extend(sub_flat.0.iter().map(|route| {
            let mut route = route.clone();
            route.host = Some(mount.host.clone());
            route
        }));
        ws_routes.extend(sub_flat.1.iter().cloned());
    }
    if let Some(overrides) = &app_config.dependency_overrides {
        for route in &mut routes {
            if !route.handler.payload.dependencies.is_empty() {
                let mut handler = (*route.handler).clone();
                handler.payload.dependency_overrides = Some(overrides.clone_ref(py));
                route.handler = Arc::new(handler);
            }
        }
        for ws in &mut ws_routes {
            ws.dependency_overrides = Some(overrides.clone_ref(py));
        }
    }
    let routes: &'static [RouteEntry] = Vec::leak(routes);

    let mut frozen_router_builder = FrozenRouterBuilder::new();
    for route in routes {
        let host: Option<Box<str>> = route.host.as_deref().map(Into::into);
        frozen_router_builder.add_route(
            route.method,
            &route.path,
            route.handler.clone(),
            host.clone(),
        );
        if route.method == HttpMethod::GET {
            frozen_router_builder.add_route(
                HttpMethod::HEAD,
                &route.path,
                route.handler.clone(),
                host,
            );
        }
    }
    let frozen_router: &'static FrozenRouter<RouteHandler> =
        Box::leak(Box::new(frozen_router_builder.build()));

    let app = Router::new();
    let app = register_routes(
        app,
        py,
        app_state,
        app_config,
        routes,
        &ws_routes,
        frozen_router,
    );
    let app = register_docs_endpoints(app, py, app_config, docs_url, openapi_url);
    middleware_stack::build_stack(app, app_config, &middlewares, app_state.event_loop)
}

fn register_routes(
    mut app: Router,
    py: Python<'_>,
    app_state: &'static AppState,
    app_config: &FastrAPI,
    routes: &'static [RouteEntry],
    ws_routes: &[WebSocketEntry],
    frozen_router: &'static FrozenRouter<RouteHandler>,
) -> Router {
    let frontend_mounts = Arc::new(app_config.frontend_mounts.clone());

    // 1. Cached routes
    app = group_and_register_routes(app, routes, |route| {
        if route.host.is_none()
            && route.handler.execution.cache_response
            && !route.path.contains('{')
            && matches!(
                route.handler.execution.execution_mode,
                ExecutionMode::SyncNoArgs
            )
        {
            let cached = precompute_const_response(py, &route.handler)?;
            Some((
                route.path.as_str(),
                cached_method_router(route.method, cached),
            ))
        } else {
            None
        }
    });

    // 2. Direct no request routes
    app = group_and_register_routes(app, routes, |route| {
        if route.host.is_none()
            && !route.handler.execution.cache_response
            && !route.path.contains('{')
            && matches!(
                route.handler.execution.execution_mode,
                ExecutionMode::SyncNoArgs | ExecutionMode::AsyncNoArgs
            )
        {
            let method_router = no_request_method_router(route.method, &route.handler, app_state);
            Some((route.path.as_str(), method_router))
        } else {
            None
        }
    });

    app = ws_routes.iter().fold(app, |current_app, ws| {
        let state = Arc::new(WsRouteState {
            handler: Arc::new(ws.handler.clone_ref(py)),
            deps: Arc::new(ws.deps.to_vec()),
            template: Arc::from(ws.path.as_str()),
            param_names: ws_param_names(&ws.path),
            event_loop: app_state.event_loop,
            dependency_overrides: ws
                .dependency_overrides
                .as_ref()
                .map(|overrides| overrides.clone_ref(py)),
        });
        let route = Router::new()
            .route(&ws.path, get(ws_handler))
            .with_state(state);
        current_app.merge(route)
    });

    app = app_config
        .static_mounts
        .iter()
        .cloned()
        .fold(app, |current_app, mount| {
            add_static_mount(current_app, mount)
        });

    app = app.method_not_allowed_fallback(move |req: Request| async move {
        method_not_allowed_response(frozen_router, &app_state.root_path, &req)
    });

    app.fallback({
        any(move |req: Request| async move {
            match dispatch_or_not_found(frozen_router, app_state, req).await {
                Ok(resp) => resp,
                Err(req) => {
                    let handler_resp = Python::attach(|py| dispatch_status_handler(py, 404));
                    match handler_resp {
                        Some(resp) => resp,
                        None => serve_frontend_mounts(frontend_mounts, req)
                            .await
                            .unwrap_or_else(not_found_response),
                    }
                }
            }
        })
    })
}

/// Returns the 404 Not Found response.
fn not_found_response() -> Response {
    (
        StatusCode::NOT_FOUND,
        axum::Json(simd_json::json!({"detail": "Not Found"})),
    )
        .into_response()
}

/// Returns the 405 Method Not Allowed response.
fn method_not_allowed_response(
    router: &FrozenRouter<RouteHandler>,
    root: &str,
    request: &Request,
) -> Response {
    let host = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(':').next().unwrap_or(value));
    let path = strip_root_path(root, request.uri().path()).unwrap_or_else(|| request.uri().path());
    let allow = router
        .allowed_methods(path, host)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let mut response =
        Python::attach(|py| dispatch_status_handler(py, StatusCode::METHOD_NOT_ALLOWED.as_u16()))
            .unwrap_or_else(|| {
                (
                    StatusCode::METHOD_NOT_ALLOWED,
                    axum::Json(simd_json::json!({"detail": "Method Not Allowed"})),
                )
                    .into_response()
            });
    if let Ok(value) = HeaderValue::from_str(&allow) {
        response.headers_mut().insert(ALLOW, value);
    }
    response
}

struct SharedSpec(Arc<JsonValue>);

impl serde::Serialize for SharedSpec {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.as_ref().serialize(serializer)
    }
}

pub(crate) fn register_docs_endpoints(
    mut app: Router,
    py: Python<'_>,
    app_config: &FastrAPI,
    docs_url: Option<&str>,
    openapi_url: Option<&str>,
) -> Router {
    let openapi_json = Arc::new(app_config.openapi_spec(py));

    if let Some(openapi_url) = openapi_url {
        app = app.route(
            openapi_url,
            get({
                let json = openapi_json;
                move || {
                    let json = json.clone();
                    async move { Json(SharedSpec(json)) }
                }
            }),
        );
    }

    if let Some(docs) = docs_url {
        let swagger_html = if let Some(params) = &app_config.swagger_ui_parameters
            && let Ok(json_str) = simd_json::to_string(&py_any_to_json(py, params.bind(py)))
        {
            include_str!("../../../static/swagger-ui.html")
                .replace("/* SWAGGER_UI_PARAMS */ {}", &json_str)
        } else {
            include_str!("../../../static/swagger-ui.html").to_string()
        };
        let swagger_html: Arc<[u8]> = Arc::from(swagger_html.into_bytes());
        app = app.route(
            docs,
            get({
                let html = swagger_html;
                move || {
                    let html = html.clone();
                    async move { Html(Bytes::from_owner(html)) }
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

    app
}

fn merge_declared_middlewares(
    py: Python<'_>,
    app_config: &FastrAPI,
    container: &mut MiddlewareContainer,
) {
    let Some(middlewares) = &app_config.middleware else {
        return;
    };

    let middlewares = middlewares.bind(py);
    let Ok(iter) = middlewares.try_iter() else {
        return;
    };

    iter.flatten().for_each(|item| {
        if let Err(err) = apply_declared_middleware(py, &item, container) {
            log_python_error("middleware setup failed", &err);
        }
    });
}

pub(crate) fn apply_declared_middleware(
    py: Python<'_>,
    middleware_item: &Bound<'_, PyAny>,
    container: &mut MiddlewareContainer,
) -> PyResult<()> {
    for builder in MIDDLEWARE_REGISTRY.builders() {
        if builder.try_from_instance(middleware_item, container)? {
            container.order.push(builder.layer());
            return Ok(());
        }
    }

    let Ok(cls) = middleware_item.getattr("cls") else {
        return Ok(());
    };
    let Ok(kwargs_any) = middleware_item.getattr("kwargs") else {
        return Ok(());
    };
    let Ok(kwargs) = kwargs_any.cast::<PyDict>() else {
        return Ok(());
    };
    let class_name_obj = cls.getattr("__name__")?;
    let class_name = class_name_obj.cast::<PyString>()?.to_str()?.to_owned();

    if let Some(builder) = MIDDLEWARE_REGISTRY.get(&class_name) {
        builder.parse_kwargs(kwargs, container)?;
        container.record_layer(&class_name);
    } else {
        let py_middleware = PyMiddleware::new(py, cls.clone().unbind());
        container.py_middlewares.push(Arc::new(py_middleware));
    }

    Ok(())
}
fn method_router<H, T>(method: HttpMethod, handler: H) -> MethodRouter
where
    H: Handler<T, ()>,
    T: 'static,
{
    let filter = match method {
        HttpMethod::GET => MethodFilter::GET.or(MethodFilter::HEAD),
        HttpMethod::POST => MethodFilter::POST,
        HttpMethod::PUT => MethodFilter::PUT,
        HttpMethod::DELETE => MethodFilter::DELETE,
        HttpMethod::PATCH => MethodFilter::PATCH,
        HttpMethod::OPTIONS => MethodFilter::OPTIONS,
        HttpMethod::HEAD => MethodFilter::HEAD,
    };
    on(filter, handler)
}

fn cached_method_router(method: HttpMethod, cached: &'static CachedResponse) -> MethodRouter {
    method_router(method, move || async move { cached.to_response() })
}

fn no_request_method_router(
    method: HttpMethod,
    handler: &'static RouteHandler,
    state: &'static AppState,
) -> MethodRouter {
    if matches!(handler.execution.execution_mode, ExecutionMode::SyncNoArgs)
        && !state.sync_to_threadpool
        && handler.response.response_class.is_none()
        && handler.response.response_model.is_none()
        && matches!(handler.response.response_type, ResponseType::Json)
    {
        return sync_no_request_method_router(method, handler);
    }

    method_router(method, move |request: Request| async move {
        let event_loop = state.event_loop;
        let (request_parts, _) = request.into_parts();
        py_bridge::scoped_request_loop(
            event_loop,
            run_py_handler_no_request(
                event_loop,
                state.sync_to_threadpool,
                handler,
                Some(request_parts),
            ),
        )
        .await
    })
}

fn sync_no_request_method_router(
    method: HttpMethod,
    handler: &'static RouteHandler,
) -> MethodRouter {
    let use_json_fast_path = handler.response.response_model.is_none()
        && handler.response.response_class.is_none()
        && matches!(handler.response.response_type, ResponseType::Json);

    method_router(method, move || async move {
        Python::attach(|py| {
            if use_json_fast_path {
                render_no_request_json_response(py, handler)
            } else {
                render_no_request_response(py, handler)
            }
        })
    })
}

fn precompute_const_response(
    py: Python<'_>,
    handler: &RouteHandler,
) -> Option<&'static CachedResponse> {
    let response = render_no_request_response(py, handler);
    let status = response.status();
    let headers = cached_headers(response.headers());
    let body = PYTHON_RUNTIME
        .block_on(to_bytes(response.into_body(), usize::MAX))
        .ok()?;

    Some(Box::leak(Box::new(CachedResponse {
        status,
        headers,
        body: Bytes::from_static(Vec::leak(body.to_vec())),
    })))
}

struct CachedResponse {
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

pub(crate) fn group_and_register_routes<'a, F>(
    mut app: Router,
    routes: &'a [RouteEntry],
    mut builder: F,
) -> Router
where
    F: FnMut(&'a RouteEntry) -> Option<(&'a str, MethodRouter)>,
{
    let mut grouped_routes: AHashMap<&'a str, MethodRouter> = AHashMap::new();
    for route in routes {
        if let Some((path, method_router)) = builder(route) {
            grouped_routes
                .entry(path)
                .and_modify(|existing| *existing = existing.clone().merge(method_router.clone()))
                .or_insert(method_router);
        }
    }
    for (path, method_router) in grouped_routes {
        app = app.route(path, method_router);
    }
    app
}

pub(crate) async fn record_prometheus_metrics(req: Request, next: Next) -> Response {
    let method = match *req.method() {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        Method::PATCH => "PATCH",
        Method::OPTIONS => "OPTIONS",
        Method::HEAD => "HEAD",
        _ => "OTHER",
    };
    let raw_uri = req.uri().clone();
    let start = Instant::now();
    let response = next.run(req).await;
    let status: metrics::SharedString = match response.status().as_u16() {
        200 => "200".into(),
        201 => "201".into(),
        204 => "204".into(),
        301 => "301".into(),
        302 => "302".into(),
        304 => "304".into(),
        307 => "307".into(),
        308 => "308".into(),
        400 => "400".into(),
        401 => "401".into(),
        403 => "403".into(),
        404 => "404".into(),
        405 => "405".into(),
        409 => "409".into(),
        413 => "413".into(),
        422 => "422".into(),
        429 => "429".into(),
        500 => "500".into(),
        502 => "502".into(),
        503 => "503".into(),
        504 => "504".into(),
        other => SharedString::from(other.to_string()),
    };

    let path: metrics::SharedString = match response.extensions().get::<RoutePattern>() {
        Some(pattern) => SharedString::from(pattern.0),
        None if response.status() == StatusCode::NOT_FOUND => SharedString::from("unmatched"),
        None => SharedString::from(raw_uri.path().to_owned()),
    };

    let elapsed = start.elapsed().as_secs_f64();

    metrics::counter!(
        "fastrapi_requests_total",
        "method" => method,
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
