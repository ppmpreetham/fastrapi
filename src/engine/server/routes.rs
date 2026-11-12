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
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, Request};
use axum::handler::Handler;
use axum::http::header::{ALLOW, CONTENT_TYPE, HOST, LOCATION};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{MethodFilter, MethodRouter, any, get, on};
use bytes::Bytes;
use percent_encoding::{CONTROLS, utf8_percent_encode};
use pyo3::prelude::*;
use simd_json::OwnedValue as JsonValue;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use crate::engine::exception_dispatch::{
    dispatch_status_handler, has_custom_handlers, resolve_pending_handler,
};
use crate::engine::executor::{
    render_no_request_json_response, render_no_request_response, run_py_handler_no_request,
};
use crate::engine::metrics::prometheus_handle;
use crate::engine::server::websocket::{
    WsRouteState, ws_handler, ws_host_handler, ws_param_names, ws_param_segments,
};
use crate::ffi::docs;
use fastrapi_core::method::{HttpMethod, allow_header};
use fastrapi_core::path::{host_without_port, join_path, strip_root_path};
use fastrapi_core::router::{FrozenRouter, FrozenRouterBuilder, RoutePattern};
use fastrapi_http::middleware::{MIDDLEWARE_REGISTRY, MiddlewareContainer};
use fastrapi_http::request_input::RequestInput;
use fastrapi_http::response_type::ResponseType;
use fastrapi_routing::route::{ExecutionMode, RouteEntry, RouteHandler, WebSocketEntry};
use fastrapi_runtime::py_bridge::{self, EventLoop};
use fastrapi_runtime::tokio_runtime::PYTHON_RUNTIME;
use metrics::SharedString;
use pyo3::types::{PyDict, PyTuple};

pub(crate) fn build_router(
    py: Python,
    app_state: &'static AppState,
    docs_url: Option<&str>,
    openapi_url: Option<&str>,
    app_config: &FastrAPI,
    custom_openapi: Option<JsonValue>,
) -> PyResult<Router> {
    let mut middlewares = app_config.middlewares.clone();
    merge_declared_middlewares(py, app_config, &mut middlewares);

    let base_router = app_config.router.bind(py);
    {
        let mut base_mut = base_router.borrow_mut();
        if base_mut
            .dependencies
            .as_ref()
            .is_none_or(|deps| deps.bind(py).len().is_ok_and(|len| len == 0))
        {
            base_mut.dependencies.clone_from(&app_config.dependencies);
        }
    }
    if let Some(handlers) = &app_config.exception_handlers
        && let Ok(dict) = handlers.bind(py).cast::<PyDict>()
    {
        app_state
            .event_loop
            .server()
            .set_exception_handlers(py, dict.clone().unbind());
    }

    let base_ref = base_router.borrow();
    base_ref.freeze(py)?;
    let base_flat = base_ref.flatten(py)?;

    let mut routes = base_flat.0.clone();
    let mut ws_routes = base_flat.1.clone();
    for mount in &app_config.app_mounts {
        let sub_flat = mounted_routes(py, mount.app.bind(py))?;
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

    for mount in &app_config.host_mounts {
        let sub_flat = mounted_routes(py, mount.app.bind(py))?;
        routes.extend(sub_flat.0.iter().map(|route| {
            let mut route = route.clone();
            route.host = Some(mount.host.clone());
            route
        }));
        ws_routes.extend(sub_flat.1.iter().map(|ws| {
            let mut ws = ws.clone();
            ws.host = Some(mount.host.clone());
            ws
        }));
    }
    let encoded_routes: Vec<RouteEntry> = routes
        .iter()
        .filter_map(|route| {
            encoded_path(&route.path).map(|path| RouteEntry {
                path,
                ..route.clone()
            })
        })
        .collect();
    routes.extend(encoded_routes);
    let encoded_ws: Vec<WebSocketEntry> = ws_routes
        .iter()
        .filter_map(|ws| encoded_path(&ws.path).map(|path| WebSocketEntry { path, ..ws.clone() }))
        .collect();
    ws_routes.extend(encoded_ws);
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
        frozen_router_builder.add_route(route.method, &route.path, route.handler.clone(), host);
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
    let app = register_docs_endpoints(app, py, app_config, docs_url, openapi_url, custom_openapi)?;
    Ok(middleware_stack::build_stack(
        app,
        app_config,
        &middlewares,
        app_state.event_loop,
    ))
}

type FlatRoutes = Arc<(Vec<RouteEntry>, Vec<WebSocketEntry>)>;

fn mounted_routes(py: Python<'_>, app: &Bound<'_, FastrAPI>) -> PyResult<FlatRoutes> {
    let app = app.borrow();
    let router = app.router.bind(py);
    {
        let mut router = router.borrow_mut();
        if router
            .dependencies
            .as_ref()
            .is_none_or(|deps| deps.bind(py).len().is_ok_and(|len| len == 0))
        {
            router.dependencies.clone_from(&app.dependencies);
        }
    }
    let router = router.borrow();
    router.freeze(py)?;
    router.flatten(py)
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
    let head_dispatch = on(MethodFilter::HEAD, move |req: Request| async move {
        dispatch_or_not_found(frozen_router, app_state, req)
            .await
            .unwrap_or_else(|_| not_found_response())
    });

    app = group_and_register_routes(app, routes, &head_dispatch, |route| {
        if route.host.is_none()
            && route.handler.execution.rate_limit_per_second.is_none()
            && route.handler.execution.cache_response
            && !route.path.contains('{')
            && matches!(
                route.handler.execution.execution_mode,
                ExecutionMode::SyncNoArgs
            )
        {
            let cached = precompute_const_response(py, &route.handler, app_state.event_loop)?;
            Some((
                route.path.as_str(),
                cached_method_router(route.method, cached),
            ))
        } else {
            None
        }
    });

    app = group_and_register_routes(app, routes, &head_dispatch, |route| {
        if route.host.is_none()
            && route.handler.execution.rate_limit_per_second.is_none()
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

    let mut ws_groups: Vec<(&str, Vec<Arc<WsRouteState>>)> = Vec::new();
    for ws in ws_routes {
        let (param_segments, segment_count) = ws_param_segments(&ws.path);
        let state = Arc::new(WsRouteState {
            handler: Arc::new(ws.handler.clone_ref(py)),
            websocket_arg: ws.websocket_arg.as_ref().map(|name| name.clone_ref(py)),
            app: app_state.app.clone_ref(py),
            params: Arc::from(ws.params.clone()),
            deps: Arc::new(ws.deps.to_vec()),
            param_segments,
            segment_count,
            param_names: ws_param_names(&ws.path),
            event_loop: app_state.event_loop,
            dependency_overrides: ws
                .dependency_overrides
                .as_ref()
                .map(|overrides| overrides.clone_ref(py)),
            override_cache: Arc::default(),
            host: ws.host.as_deref().map(Into::into),
        });
        match ws_groups.iter_mut().find(|(path, _)| *path == ws.path) {
            Some((_, group)) => group.push(state),
            None => ws_groups.push((ws.path.as_str(), vec![state])),
        }
    }
    for (path, mut group) in ws_groups {
        app = if let [state] = group.as_slice()
            && state.host.is_none()
        {
            app.merge(
                Router::new()
                    .route(path, get(ws_handler))
                    .with_state(state.clone()),
            )
        } else {
            group.sort_by_key(|state| state.host.is_none());
            let group: Arc<[Arc<WsRouteState>]> = group.into();
            app.merge(
                Router::new()
                    .route(path, get(ws_host_handler))
                    .with_state(group),
            )
        };
    }

    let handlers = has_custom_handlers(py, app_state.event_loop).then_some(app_state.event_loop);
    app = app_config
        .static_mounts
        .iter()
        .cloned()
        .fold(app, |current_app, mount| {
            add_static_mount(current_app, mount, handlers)
        });

    app = app.method_not_allowed_fallback(move |req: Request| async move {
        let allow = allowed_methods_header(frozen_router, &app_state.root_path, &req);
        let handled = status_handler(app_state.event_loop, StatusCode::METHOD_NOT_ALLOWED, &req);
        method_not_allowed_response(handled, allow).await
    });

    app.fallback({
        any(move |req: Request| async move {
            match dispatch_or_not_found(frozen_router, app_state, req).await {
                Ok(resp) => resp,
                Err(req) => {
                    if let Some(redirect) = slash_redirect(frozen_router, app_state, &req) {
                        return redirect;
                    }
                    let handler_resp =
                        status_handler(app_state.event_loop, StatusCode::NOT_FOUND, &req);
                    match handler_resp {
                        Some(resp) => resolve_pending_handler(resp).await,
                        None => serve_frontend_mounts(frontend_mounts, req)
                            .await
                            .unwrap_or_else(not_found_response),
                    }
                }
            }
        })
    })
}

fn slash_redirect(
    router: &FrozenRouter<RouteHandler>,
    state: &AppState,
    request: &Request,
) -> Option<Response> {
    let path = request.uri().path();
    if !state.redirect_slashes || path == "/" {
        return None;
    }
    let toggled = path
        .strip_suffix('/')
        .map_or_else(|| format!("{path}/"), str::to_owned);
    let host = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok());
    let lookup = strip_root_path(&state.root_path, &toggled).unwrap_or(&toggled);
    let host_name = host.map(host_without_port);
    if router.allowed_methods(lookup, host_name) == 0 {
        return None;
    }
    let mut location = host.map_or_else(
        || toggled.clone(),
        |host| format!("{}://{host}{toggled}", request_scheme(request)),
    );
    if let Some(query) = request.uri().query() {
        location.push('?');
        location.push_str(query);
    }
    let location = HeaderValue::from_str(&location).ok()?;
    Some((StatusCode::TEMPORARY_REDIRECT, [(LOCATION, location)]).into_response())
}

fn request_scheme(request: &Request) -> &str {
    if let Some(scheme) = request.uri().scheme_str() {
        return scheme;
    }
    let trusted_proxy = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|ConnectInfo(addr)| addr.ip().is_loopback());
    request
        .headers()
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .filter(|scheme| trusted_proxy && matches!(*scheme, "http" | "https"))
        .unwrap_or("http")
}

/// Returns the 404 Not Found response.
fn json_body(status: StatusCode, body: Bytes) -> Response {
    (
        status,
        [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
        body,
    )
        .into_response()
}

fn not_found_response() -> Response {
    json_body(
        StatusCode::NOT_FOUND,
        Bytes::from_static(br#"{"detail":"Not Found"}"#),
    )
}

/// Returns the 405 Method Not Allowed response.
fn allowed_methods_header(
    router: &FrozenRouter<RouteHandler>,
    root: &str,
    request: &Request,
) -> u8 {
    let host = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .map(host_without_port);
    let path = strip_root_path(root, request.uri().path()).unwrap_or_else(|| request.uri().path());
    router.allowed_methods(path, host)
}

pub(crate) fn status_handler(
    server: EventLoop,
    status: StatusCode,
    request: &Request,
) -> Option<Response> {
    let input = RequestInput::new(
        request.method(),
        request.uri(),
        request.headers(),
        request.extensions(),
        Some(server),
    );
    dispatch_status_handler(server, status.as_u16(), Some(&input))
}

pub(crate) async fn method_not_allowed_response(handled: Option<Response>, allow: u8) -> Response {
    let mut response = match handled {
        Some(resp) => resolve_pending_handler(resp).await,
        None => json_body(
            StatusCode::METHOD_NOT_ALLOWED,
            Bytes::from_static(br#"{"detail":"Method Not Allowed"}"#),
        ),
    };
    response.headers_mut().insert(ALLOW, allow_header(allow));
    response
}

pub(crate) fn register_docs_endpoints(
    mut app: Router,
    py: Python<'_>,
    app_config: &FastrAPI,
    docs_url: Option<&str>,
    openapi_url: Option<&str>,
    custom_openapi: Option<JsonValue>,
) -> PyResult<Router> {
    let Some(openapi_url) = openapi_url else {
        return Ok(register_metrics_endpoint(app, app_config));
    };
    let openapi_json = match custom_openapi {
        Some(spec) => spec,
        None => match app_config.openapi_schema.lock().as_ref() {
            Some(schema) => fastrapi_pyutil::json::py_any_to_json(py, schema.bind(py)),
            None => app_config.openapi_spec(py)?,
        },
    };
    let openapi_body = serde_json::to_vec(&openapi_json).ok().map(Bytes::from);
    app = app.route(
        openapi_url,
        get(move || {
            let body = openapi_body.clone();
            async move {
                body.map_or_else(
                    || StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    |body| json_body(StatusCode::OK, body),
                )
            }
        }),
    );
    let root_path = app_config.root_path.trim_end_matches('/');
    let spec_url = format!("{root_path}{openapi_url}");
    let title = &app_config.title;
    if let Some(docs) = docs_url {
        let swagger_ui_parameters = app_config
            .swagger_ui_parameters
            .as_ref()
            .map(|params| params.bind(py));
        let init_oauth = app_config
            .swagger_ui_init_oauth
            .as_ref()
            .map(|init| init.bind(py));
        let oauth2_redirect_url = app_config
            .swagger_ui_oauth2_redirect_url
            .as_deref()
            .map(|url| format!("{root_path}{url}"));
        let page = docs::SwaggerPage {
            openapi_url: &spec_url,
            title: &format!("{title} - Swagger UI"),
            swagger_js_url: docs::SWAGGER_JS_URL,
            swagger_css_url: docs::SWAGGER_CSS_URL,
            swagger_favicon_url: docs::FAVICON_URL,
            oauth2_redirect_url: oauth2_redirect_url.as_deref(),
            init_oauth,
            swagger_ui_parameters,
        };
        match docs::swagger_ui_page(py, &page) {
            Ok(html) => app = html_route(app, docs, html),
            Err(err) => log_python_error("Swagger UI page setup failed", &err),
        }
        if let Some(redirect) = &app_config.swagger_ui_oauth2_redirect_url {
            app = html_route(app, redirect, docs::OAUTH2_REDIRECT_HTML.to_owned());
        }
    }
    if let Some(redoc) = &app_config.redoc_url {
        let html = docs::redoc_page(
            &spec_url,
            &format!("{title} - ReDoc"),
            docs::REDOC_JS_URL,
            docs::FAVICON_URL,
            true,
        );
        app = html_route(app, redoc, html);
    }
    if let Some(scalar) = &app_config.scalar_url {
        let html = include_str!("../../../static/scalar.html").replace(DOCS_SPEC_URL, &spec_url);
        app = html_route(app, scalar, html);
    }
    if let Some(elements) = &app_config.elements_url {
        let html = include_str!("../../../static/elements.html").replace(DOCS_SPEC_URL, &spec_url);
        app = html_route(app, elements, html);
    }

    Ok(register_metrics_endpoint(app, app_config))
}

pub(crate) fn custom_openapi_spec(py: Python<'_>, app: &Bound<'_, PyAny>) -> Option<JsonValue> {
    let openapi = app
        .getattr(pyo3::intern!(py, "__dict__"))
        .ok()?
        .get_item("openapi")
        .ok()?;
    match openapi.call0() {
        Ok(schema) => Some(fastrapi_pyutil::json::py_any_to_json(py, &schema)),
        Err(err) => {
            log_python_error("Custom openapi() failed", &err);
            None
        }
    }
}

const DOCS_SPEC_URL: &str = "/api-docs/openapi.json";

fn html_route(app: Router, path: &str, html: String) -> Router {
    let html: Arc<[u8]> = Arc::from(html.into_bytes());
    app.route(
        path,
        get(move || {
            let html = html.clone();
            async move { Html(Bytes::from_owner(html)) }
        }),
    )
}

fn register_metrics_endpoint(mut app: Router, app_config: &FastrAPI) -> Router {
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

    let registered = container.order.len();
    iter.flatten().for_each(|item| {
        if let Err(err) = apply_declared_middleware(py, &item, container) {
            log_python_error("middleware setup failed", &err);
        }
    });
    let mut declared = container.order.split_off(registered);
    declared.reverse();
    declared.append(&mut container.order);
    container.order = declared;
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
    let args = middleware_item
        .getattr("args")
        .and_then(|args| args.cast_into::<PyTuple>().map_err(Into::into))
        .unwrap_or_else(|_| PyTuple::empty(py));
    container.register(&cls, &args, kwargs)
}
fn method_router<H, T>(method: HttpMethod, handler: H) -> MethodRouter
where
    H: Handler<T, ()>,
    T: 'static,
{
    let filter = match method {
        HttpMethod::GET => MethodFilter::GET,
        HttpMethod::POST => MethodFilter::POST,
        HttpMethod::PUT => MethodFilter::PUT,
        HttpMethod::DELETE => MethodFilter::DELETE,
        HttpMethod::PATCH => MethodFilter::PATCH,
        HttpMethod::OPTIONS => MethodFilter::OPTIONS,
        HttpMethod::HEAD => MethodFilter::HEAD,
        HttpMethod::TRACE => MethodFilter::TRACE,
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
        return sync_no_request_method_router(method, handler, state.event_loop);
    }

    method_router(method, move |request: Request| async move {
        let event_loop = state.event_loop;
        let context = request
            .extensions()
            .get::<py_bridge::RequestContext>()
            .cloned();
        let (request_parts, _) = request.into_parts();
        let response = py_bridge::scoped_request_loop(
            event_loop,
            context,
            run_py_handler_no_request(
                event_loop,
                state.sync_to_threadpool,
                handler,
                Some(request_parts),
            ),
        )
        .await;
        resolve_pending_handler(response).await
    })
}

fn sync_no_request_method_router(
    method: HttpMethod,
    handler: &'static RouteHandler,
    server: EventLoop,
) -> MethodRouter {
    method_router(method, move |request: Request| async move {
        let context = request
            .extensions()
            .get::<py_bridge::RequestContext>()
            .cloned();
        py_bridge::scoped_request_context(context, async move {
            let input = RequestInput::new(
                request.method(),
                request.uri(),
                request.headers(),
                request.extensions(),
                Some(server),
            );
            let response = Python::attach(|py| {
                let _entered = py_bridge::enter_request_copy(py);
                render_no_request_json_response(py, handler, &input, server)
            });
            resolve_pending_handler(response).await
        })
        .await
    })
}

fn precompute_const_response(
    py: Python<'_>,
    handler: &RouteHandler,
    server: EventLoop,
) -> Option<&'static CachedResponse> {
    let response = render_no_request_response(py, handler, server);
    let status = response.status();
    let headers = cached_headers(response.headers());
    let body = PYTHON_RUNTIME
        .block_on(to_bytes(response.into_body(), usize::MAX))
        .ok()?;

    Some(Box::leak(Box::new(CachedResponse {
        status,
        headers,
        body,
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
    head_dispatch: &MethodRouter,
    mut builder: F,
) -> Router
where
    F: FnMut(&'a RouteEntry) -> Option<(&'a str, MethodRouter)>,
{
    let mut grouped_routes: AHashMap<&'a str, (MethodRouter, bool, bool)> = AHashMap::new();
    for route in routes {
        if let Some((path, method_router)) = builder(route) {
            let is_get = route.method == HttpMethod::GET;
            let is_head = route.method == HttpMethod::HEAD;
            grouped_routes
                .entry(path)
                .and_modify(|(existing, has_get, has_head)| {
                    *existing = existing.clone().merge(method_router.clone());
                    *has_get |= is_get;
                    *has_head |= is_head;
                })
                .or_insert((method_router, is_get, is_head));
        }
    }
    for (path, (method_router, has_get, has_head)) in grouped_routes {
        let method_router = if has_get && !has_head {
            method_router.merge(head_dispatch.clone())
        } else {
            method_router
        };
        app = app.route(path, method_router);
    }
    app
}

fn encoded_path(path: &str) -> Option<String> {
    (!path.is_ascii()).then(|| utf8_percent_encode(path, CONTROLS).to_string())
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

    let path = SharedString::from(
        response
            .extensions()
            .get::<RoutePattern>()
            .map_or("unmatched", |pattern| pattern.0),
    );

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
