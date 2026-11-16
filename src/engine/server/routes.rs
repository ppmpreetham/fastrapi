//! The construction of the axum router from the routes of the app.
//! It registers the route handlers, the documentation endpoints and the cached responses.
//! It also contains the 404 and 405 responses.

use super::custom_route::install_custom_handlers;
use super::dispatch::dispatch_or_not_found;
use super::files::{add_static_mount, serve_frontend_mounts};
use super::middleware_stack;
use super::serve::AppState;
use crate::engine::handler_errors::log_python_error;

use crate::engine::config::FastrAPI;
use ahash::AHashMap;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, Extension, OriginalUri, Request};
use axum::handler::Handler;
use axum::http::header::{CONTENT_TYPE, HOST, LOCATION};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{MethodFilter, MethodRouter, get, on};
use bytes::Bytes;
use fastrapi_core::connection::PeerAddrs;
use percent_encoding::{CONTROLS, utf8_percent_encode};
use pyo3::intern;
use pyo3::prelude::*;
use regex::{Captures, Regex, RegexBuilder};
use simd_json::OwnedValue as JsonValue;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Instant;
use tower::ServiceExt;
use tracing::error;

use crate::engine::exception_dispatch::{
    RoutingErrorHandlers, dispatch_status_handler, has_custom_handlers, invalidate_handler_cache,
    resolve_pending_handler,
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
use fastrapi_core::path::{host_without_port, strip_root_path};
use fastrapi_core::router::{FrozenRouter, FrozenRouterBuilder, RoutePattern};
use fastrapi_http::middleware::asgi::mounted_asgi;
use fastrapi_http::middleware::{DeclaredLayer, MiddlewareContainer};
use fastrapi_http::request_input::{AsgiMount, HostPathParams, RequestInput};
use fastrapi_http::response_type::ResponseType;
use fastrapi_openapi::spec::with_root_path_server;
use fastrapi_routing::route::{ExecutionMode, RouteEntry, RouteHandler, WebSocketEntry};
use fastrapi_runtime::py_bridge::{self, EventLoop};
use metrics::SharedString;
use pyo3::types::{PyDict, PyTuple};

pub(crate) fn build_router(
    py: Python,
    app_state: &'static AppState,
    docs_url: Option<&str>,
    openapi_url: Option<&str>,
    app_config: &FastrAPI,
    custom_openapi: Option<JsonValue>,
    operation_spans: bool,
) -> PyResult<Router> {
    let mut middlewares = app_config.middlewares.clone();
    merge_declared_middlewares(py, app_config, &mut middlewares)?;

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
            .set_exception_handlers(dict.clone().unbind());
        invalidate_handler_cache();
        _ = app_state
            .routing_handlers
            .set(RoutingErrorHandlers::resolve(py, dict)?);
    }

    let base_ref = base_router.borrow();
    base_ref.freeze(py)?;
    let base_flat = base_ref.flatten(py)?;

    let mut routes = base_flat.0.clone();
    let mut ws_routes = base_flat.1.clone();
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
    if operation_spans {
        for route in &mut routes {
            let mut handler = (*route.handler).clone();
            handler.execution.operation_spans = true;
            handler.execution.execution_mode = ExecutionMode::Managed;
            route.handler = Arc::new(handler);
        }
    }
    let routes: &'static [RouteEntry] = Vec::leak(routes);

    let mut route_methods: AHashMap<RouteKey<'_>, u8> = AHashMap::new();
    for route in routes {
        *route_methods.entry(RouteKey::of(route)).or_default() |= route.method.bit();
    }
    let mut frozen_router_builder = FrozenRouterBuilder::new();
    for route in routes {
        let host: Option<Box<str>> = route.host.as_deref().map(Into::into);
        let allow = route_methods[&RouteKey::of(route)];
        frozen_router_builder.add_route(
            route.method,
            &route.path,
            route.handler.clone(),
            host,
            allow,
        );
    }
    let frozen_router: &'static FrozenRouter<RouteHandler> =
        Box::leak(Box::new(frozen_router_builder.build()));
    _ = app_state.router.set(frozen_router);
    install_custom_handlers(py, routes, frozen_router, app_state)?;

    let host_apps = host_apps(py, app_state, app_config)?;
    let app = Router::new();
    let app = register_routes(
        app,
        py,
        app_state,
        app_config,
        routes,
        &ws_routes,
        frozen_router,
        host_apps,
    );
    let app = register_docs_endpoints(app, py, app_config, docs_url, openapi_url, custom_openapi)?;
    let app = nest_sub_apps(py, app, app_state, app_config)?;
    middleware_stack::build_stack(app, app_config, &middlewares, app_state.event_loop)
}

/// What the entries of one `APIRoute` share: fastrapi keeps an entry for each
/// of its methods, all with the route's path, host and endpoint.
#[derive(PartialEq, Eq, Hash)]
struct RouteKey<'a> {
    path: &'a str,
    host: Option<&'a str>,
    endpoint: *mut pyo3::ffi::PyObject,
}

impl<'a> RouteKey<'a> {
    fn of(route: &'a RouteEntry) -> Self {
        Self {
            path: &route.path,
            host: route.host.as_deref(),
            endpoint: route.handler.execution.func.as_ptr(),
        }
    }
}

fn nest_sub_apps(
    py: Python<'_>,
    app: Router,
    parent: &'static AppState,
    app_config: &FastrAPI,
) -> PyResult<Router> {
    app_config.app_mounts.iter().try_fold(app, |app, mount| {
        let prefix: Arc<str> = mount.path.as_str().into();
        let sub_app = sub_app_router(py, parent, &mount.app)?.layer(axum::middleware::map_request(
            move |mut request: Request| {
                mark_mounted(&mut request, &prefix);
                async move { request }
            },
        ));
        Ok(app.nest_service(&mount.path, sub_app))
    })
}

/// An app mounted with `app.host()`, which serves every request for its host
/// that the parent app has no route for, like Starlette's `Host` route.
struct HostApp {
    pattern: Regex,
    convertors: Option<Py<PyDict>>,
    router: Router,
}

impl HostApp {
    /// Compiles the host pattern with Starlette's `compile_path`, like its `Host` route.
    fn new(py: Python<'_>, host: &str, router: Router) -> PyResult<Self> {
        let compiled = STARLETTE_COMPILE_PATH.get(py)?.call1((host,))?;
        let regex = compiled.get_item(0)?.getattr(intern!(py, "pattern"))?;
        let pattern = RegexBuilder::new(regex.extract::<&str>()?)
            .build()
            .map_err(|err| pyo3::exceptions::PyValueError::new_err(err.to_string()))?;
        let convertors = compiled.get_item(2)?.cast_into::<PyDict>()?;
        Ok(Self {
            pattern,
            convertors: (!convertors.is_empty()).then(|| convertors.unbind()),
            router,
        })
    }

    fn path_params(&self, captures: &Captures<'_>) -> PyResult<Option<HostPathParams>> {
        let Some(convertors) = &self.convertors else {
            return Ok(None);
        };
        Python::attach(|py| {
            let params = PyDict::new(py);
            for (name, convertor) in convertors.bind(py) {
                let value = captures
                    .name(name.extract::<&str>()?)
                    .map_or("", |found| found.as_str());
                params.set_item(
                    &name,
                    convertor.call_method1(intern!(py, "convert"), (value,))?,
                )?;
            }
            Ok(Some(HostPathParams(params.unbind())))
        })
    }
}

fastrapi_pyutil::cached_py_import!(STARLETTE_COMPILE_PATH, "starlette.routing", "compile_path");

fn host_apps(
    py: Python<'_>,
    parent: &'static AppState,
    app_config: &FastrAPI,
) -> PyResult<&'static [HostApp]> {
    let apps = app_config
        .host_mounts
        .iter()
        .map(|mount| HostApp::new(py, &mount.host, sub_app_router(py, parent, &mount.app)?))
        .collect::<PyResult<Vec<_>>>()?;
    Ok(Vec::leak(apps))
}

async fn serve_host_app(
    host_apps: &'static [HostApp],
    mut request: Request,
) -> Result<Response, Request> {
    if host_apps.is_empty() {
        return Err(request);
    }
    let Some(host) = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .map(host_without_port)
    else {
        return Err(request);
    };
    let Some((app, captures)) = host_apps
        .iter()
        .find_map(|app| app.pattern.captures(host).map(|captures| (app, captures)))
    else {
        return Err(request);
    };
    match app.path_params(&captures) {
        Ok(Some(params)) => {
            request.extensions_mut().insert(params);
        }
        Ok(None) => {}
        Err(err) => Python::attach(|py| err.print(py)),
    }
    match app.router.clone().oneshot(request).await {
        Ok(response) => Ok(response),
        Err(never) => match never {},
    }
}

fn sub_app_router(
    py: Python<'_>,
    parent: &'static AppState,
    app: &Py<FastrAPI>,
) -> PyResult<Router> {
    let bound = app.bind(py);
    bound.borrow_mut().apply_router_mounts(py)?;
    let event_loop = parent.event_loop.sibling(py);
    event_loop.server().set_app(app.clone_ref(py).into_any());
    let custom_openapi = custom_openapi_spec(py, bound.as_any());
    let config = bound.borrow();
    let state = AppState::of(py, event_loop, app, &config).leak();
    build_router(
        py,
        state,
        config.docs_url.as_deref(),
        config.openapi_url.as_deref(),
        &config,
        custom_openapi,
        false,
    )
}

fn mark_mounted(request: &mut Request, prefix: &str) {
    let path = request.extensions().get::<OriginalUri>().map_or_else(
        || request.uri().path().into(),
        |original| original.path().into(),
    );
    request.extensions_mut().insert(AsgiMount {
        root_path: prefix.into(),
        path,
        app_root_path: None,
        router: None,
    });
}

fn register_routes(
    mut app: Router,
    py: Python<'_>,
    app_state: &'static AppState,
    app_config: &FastrAPI,
    routes: &'static [RouteEntry],
    ws_routes: &[WebSocketEntry],
    frozen_router: &'static FrozenRouter<RouteHandler>,
    host_apps: &'static [HostApp],
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
            && route.handler.execution.custom_handler.get().is_none()
            && route.handler.execution.cache_response
            && !route.path.contains('{')
            && matches!(
                route.handler.execution.execution_mode,
                ExecutionMode::SyncNoArgs
            )
        {
            Some((
                route.path.as_str(),
                cached_method_router(route.method, &route.handler, app_state.event_loop),
            ))
        } else {
            None
        }
    });

    app = group_and_register_routes(app, routes, &head_dispatch, |route| {
        if route.host.is_none()
            && route.handler.execution.rate_limit_per_second.is_none()
            && route.handler.execution.custom_handler.get().is_none()
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
    let max_body_size = app_state.max_body_size.unwrap_or(usize::MAX);
    app = app_config
        .asgi_mounts
        .iter()
        .fold(app, |current_app, mount| {
            let mounted = Arc::clone(&mount.app);
            let prefix: Arc<str> = mount.path.as_str().into();
            current_app.nest_service(
                &mount.path,
                tower::service_fn(move |req: Request| {
                    let response = mounted_asgi(
                        Arc::clone(&mounted),
                        Arc::clone(&prefix),
                        req,
                        app_state.event_loop,
                        max_body_size,
                    );
                    async move { Ok::<_, std::convert::Infallible>(response.await) }
                }),
            )
        });

    // A route registered directly in axum answers only its own methods, so a
    // request for another method on its path goes through the frozen router too.
    let unmatched = move |req: Request| async move {
        let req = match dispatch_or_not_found(frozen_router, app_state, req).await {
            Ok(response) => return response,
            Err(req) => req,
        };
        let req = match serve_host_app(host_apps, req).await {
            Ok(response) => return response,
            Err(req) => req,
        };
        if let Some(redirect) = slash_redirect(frozen_router, app_state, &req) {
            return redirect;
        }
        let handled = status_handler(app_state, StatusCode::NOT_FOUND, None, &req);
        match handled {
            Some(response) => resolve_pending_handler(response).await,
            None => serve_frontend_mounts(frontend_mounts, req)
                .await
                .unwrap_or_else(not_found_response),
        }
    };
    app.method_not_allowed_fallback(unmatched.clone())
        .fallback(unmatched)
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

pub(crate) fn request_scheme(request: &Request) -> &str {
    if let Some(scheme) = request.uri().scheme_str() {
        return scheme;
    }
    let trusted_proxy = request
        .extensions()
        .get::<ConnectInfo<PeerAddrs>>()
        .is_some_and(|ConnectInfo(peer)| peer.remote.ip().is_loopback());
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

/// Runs the exception handler that the app resolved for its router's 404 or 405, if any.
fn status_handler(
    state: &AppState,
    status: StatusCode,
    allow: Option<&HeaderValue>,
    request: &Request,
) -> Option<Response> {
    let handler = state.routing_handlers.get()?.for_status(status)?;
    let input = RequestInput::new(
        request.method(),
        request.uri(),
        request.headers(),
        request.extensions(),
        Some(state.event_loop),
    );
    dispatch_status_handler(state.event_loop, handler, status, allow, &input)
}

/// Returns the 405 response for a path whose first matching route has the methods in `allow`.
/// Like Starlette, a custom handler gets the `Allow` header on its `HTTPException`
/// and answers in place of the default response. The caller adds `Allow` to the final response.
pub(crate) fn method_not_allowed_response(
    state: &AppState,
    request: &Request,
    allow: u8,
) -> Response {
    let allow = allow_header(allow);
    status_handler(state, StatusCode::METHOD_NOT_ALLOWED, Some(&allow), request).unwrap_or_else(
        || {
            (
                StatusCode::METHOD_NOT_ALLOWED,
                [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
                Bytes::from_static(br#"{"detail":"Method Not Allowed"}"#),
            )
                .into_response()
        },
    )
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
        return register_metrics_endpoint(app, app_config);
    };
    let openapi_json = match custom_openapi {
        Some(spec) => spec,
        None => match app_config.openapi_schema.lock().as_ref() {
            Some(schema) => fastrapi_pyutil::json::py_any_to_json(py, schema.bind(py)),
            None => app_config.openapi_spec(py)?,
        },
    };
    let openapi_body = serde_json::to_vec(&openapi_json).ok().map(Bytes::from);
    let root_path_in_servers = app_config.root_path_in_servers;
    app = app.route(
        openapi_url,
        get(move |mount: Option<Extension<AsgiMount>>| {
            let body = mount
                .filter(|_| root_path_in_servers)
                .and_then(|Extension(mount)| with_root_path_server(&openapi_json, &mount.root_path))
                .map_or_else(
                    || openapi_body.clone(),
                    |document| serde_json::to_vec(&document).ok().map(Bytes::from),
                );
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
            Err(err) => log_python_error(py, "Swagger UI page setup failed", &err),
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

    register_metrics_endpoint(app, app_config)
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
            log_python_error(py, "Custom openapi() failed", &err);
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

fn register_metrics_endpoint(mut app: Router, app_config: &FastrAPI) -> PyResult<Router> {
    if let Some(config) = &app_config.prometheus_config {
        let handle = prometheus_handle()?;
        app = app.route(
            &config.metrics_path,
            get(move || {
                let handle = handle.clone();
                async move { handle.render() }
            }),
        );
    }

    Ok(app)
}

/// Registers the `middleware=[...]` list of the app. Like Starlette's
/// `build_middleware_stack`, a malformed entry raises instead of being skipped.
fn merge_declared_middlewares(
    py: Python<'_>,
    app_config: &FastrAPI,
    container: &mut MiddlewareContainer,
) -> PyResult<()> {
    let Some(middlewares) = &app_config.middleware else {
        return Ok(());
    };

    let registered = container.order.len();
    for item in middlewares.bind(py).try_iter()? {
        apply_declared_middleware(py, &item?, container)?;
    }
    let mut declared = container.order.split_off(registered);
    declared.reverse();
    declared.append(&mut container.order);
    container.order = declared;
    Ok(())
}

pub(crate) fn apply_declared_middleware(
    py: Python<'_>,
    middleware_item: &Bound<'_, PyAny>,
    container: &mut MiddlewareContainer,
) -> PyResult<()> {
    if let Some(layer) = DeclaredLayer::from_instance(middleware_item)? {
        container.order.push(layer);
        return Ok(());
    }

    let cls = middleware_item.getattr(intern!(py, "cls"))?;
    let kwargs_any = middleware_item.getattr(intern!(py, "kwargs"))?;
    let kwargs = kwargs_any.cast::<PyDict>()?;
    let args = middleware_item
        .getattr(intern!(py, "args"))?
        .cast_into::<PyTuple>()?;
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

/// Serves a `cache_response` route. The response is rendered on the first
/// request, so after the lifespan startup has run, and kept only when it is a
/// 2xx: an error response is never cached and the handler runs again.
fn cached_method_router(
    method: HttpMethod,
    handler: &'static RouteHandler,
    server: EventLoop,
) -> MethodRouter {
    let cell: &'static OnceLock<CachedResponse> = Box::leak(Box::new(OnceLock::new()));
    method_router(method, move || async move {
        if let Some(cached) = cell.get() {
            return cached.to_response();
        }
        render_cached_response(handler, server, cell).await
    })
}

async fn render_cached_response(
    handler: &RouteHandler,
    server: EventLoop,
    cell: &'static OnceLock<CachedResponse>,
) -> Response {
    let response = Python::attach(|py| render_no_request_response(py, handler, server));
    if !response.status().is_success() {
        return response;
    }
    let (parts, body) = response.into_parts();
    let body = match to_bytes(body, usize::MAX).await {
        Ok(body) => body,
        Err(err) => {
            error!("Failed to buffer the cached response: {err}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let cached = cell.get_or_init(|| CachedResponse {
        status: parts.status,
        headers: cached_headers(&parts.headers),
        body,
    });
    cached.to_response()
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
