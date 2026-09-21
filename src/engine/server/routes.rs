use super::dispatch::*;
use super::files::*;
use super::lifecycle::*;
use super::middleware_stack;
use super::serve::*;

use crate::engine::types::FastrAPI;
use ahash::AHashMap;
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::Request,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header::CONTENT_TYPE},
    middleware::Next,
    response::{Html, IntoResponse, Response},
    routing::{MethodRouter, *},
};
use bytes::Bytes;
use pyo3::prelude::*;
use simd_json::OwnedValue as JsonValue;
use std::sync::Arc;
use std::time::Instant;

use crate::match_method_router;
use crate::{
    engine::metrics::prometheus_handle,
    globals::PYTHON_RUNTIME,
    http::{
        middleware::{MIDDLEWARE_REGISTRY, MiddlewareContainer, PyMiddleware},
        websocket::{WsRouteState, ws_handler},
    },
    routing::{
        router::{FrozenRouter, FrozenRouterBuilder},
        types::{HttpMethod, RouteEntry, RouteHandler, WebSocketEntry},
    },
    runtime::executor::{
        ExecutionMode, render_no_request_json_response, render_no_request_response,
        run_py_handler_no_request,
    },
    runtime::py_bridge,
    utils::{openapi::build_openapi_spec, py_any_to_json},
};

pub(crate) fn build_router(
    py: Python,
    app_state: AppState,
    docs_url: Option<String>,
    openapi_url: String,
    app_config: &FastrAPI,
) -> Router {
    let mut middlewares = app_config.middlewares.clone();
    merge_declared_middlewares(py, app_config, &mut middlewares);

    let base_router = app_config.router.bind(py);
    {
        let mut base_mut = base_router.borrow_mut();
        if base_mut.dependencies.is_none() {
            base_mut.dependencies = app_config.dependencies.clone();
        }
    }
    if let Some(overrides) = &app_config.dependency_overrides {
        let items = overrides.bind(py).call_method0("items");
        let map: ahash::AHashMap<u64, Py<PyAny>> = items
            .ok()
            .and_then(|items| items.try_iter().ok())
            .into_iter()
            .flatten()
            .filter_map(|item| {
                let item = item.ok()?;
                let pair = item.cast::<pyo3::types::PyTuple>().ok()?;
                let key = pair.get_item(0).ok()?;
                let value = pair.get_item(1).ok()?;
                Some((key.as_ptr() as u64, value.unbind()))
            })
            .collect();
        crate::globals::set_dependency_overrides(map);
    }

    if let Some(handlers) = &app_config.exception_handlers
        && let Ok(dict) = handlers.bind(py).cast::<pyo3::types::PyDict>()
    {
        crate::globals::set_exception_handlers(dict.clone().unbind());
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
                sub_mut.dependencies = sub_ref.dependencies.clone();
            }
        }
        sub_router.borrow().freeze(py);
        let sub_flat = sub_router.borrow().flatten(py);

        routes.extend(sub_flat.0.iter().map(|route| {
            let mut route = route.clone();
            route.path = crate::routing::tree::join_path(&mount.path, &route.path);
            route
        }));
        ws_routes.extend(sub_flat.1.iter().map(|ws| {
            let mut ws = ws.clone();
            ws.path = crate::routing::tree::join_path(&mount.path, &ws.path);
            ws
        }));
    }
    let flat = (routes, ws_routes);

    let mut frozen_router_builder = FrozenRouterBuilder::new();
    flat.0.iter().for_each(|route| {
        frozen_router_builder.add_route(route.method, route.path.clone(), route.handler.clone());
        if route.method == HttpMethod::GET {
            frozen_router_builder.add_route(
                HttpMethod::HEAD,
                route.path.clone(),
                route.handler.clone(),
            );
        }
    });
    let frozen_router = Arc::new(frozen_router_builder.build());

    let app = Router::new();
    let app = register_routes(app, py, &app_state, app_config, &flat, frozen_router);
    let app = register_docs_endpoints(app, py, app_config, docs_url.as_deref(), &openapi_url);
    middleware_stack::build_stack(app, app_config, &middlewares, app_state.pick_loop())
}

fn register_routes(
    mut app: Router,
    py: Python<'_>,
    app_state: &AppState,
    app_config: &FastrAPI,
    flat: &(Vec<RouteEntry>, Vec<WebSocketEntry>),
    frozen_router: Arc<FrozenRouter>,
) -> Router {
    let frontend_mounts = Arc::new(app_config.frontend_mounts.clone());

    // 1. Cached routes
    app = group_and_register_routes(app, &flat.0, |route| {
        if route.handler.execution.cache_response
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
    app = group_and_register_routes(app, &flat.0, |route| {
        if !route.handler.execution.cache_response
            && !route.path.contains('{')
            && matches!(
                route.handler.execution.execution_mode,
                ExecutionMode::SyncNoArgs | ExecutionMode::AsyncNoArgs
            )
        {
            let method_router =
                no_request_method_router(route.method, route.handler.clone(), app_state.clone());
            Some((route.path.as_str(), method_router))
        } else {
            None
        }
    });

    app = flat.1.iter().fold(app, |current_app, ws| {
        let state = Arc::new(WsRouteState {
            handler: Arc::new(ws.handler.clone_ref(py)),
            deps: Arc::new(ws.deps.to_vec()),
            template: Arc::from(ws.path.as_str()),
            param_names: crate::http::websocket::ws_param_names(&ws.path),
            async_loop: app_state.pick_loop(),
        });
        let route = Router::new()
            .route(&ws.path, axum::routing::get(ws_handler))
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

    app = app.method_not_allowed_fallback(|| async { method_not_allowed_response() });

    app.fallback({
        let router = frozen_router;
        let state = app_state.clone();
        axum::routing::any(move |req: Request| async move {
            match dispatch_or_not_found(router.clone(), state.clone(), req).await {
                Ok(resp) => resp,
                Err(req) => {
                    let handler_resp = Python::attach(|py| {
                        crate::engine::errors::dispatch_status_handler(py, 404)
                    });
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

/// 404
fn not_found_response() -> Response {
    (
        StatusCode::NOT_FOUND,
        axum::Json(simd_json::json!({"detail": "Not Found"})),
    )
        .into_response()
}

/// 405
fn method_not_allowed_response() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        axum::Json(simd_json::json!({"detail": "Method Not Allowed"})),
    )
        .into_response()
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
    openapi_url: &str,
) -> Router {
    let openapi_json = Arc::new(build_openapi_spec(py, app_config));

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
            log_python_error("middleware setup failed", err);
        }
    });
}

pub(crate) fn apply_declared_middleware(
    _py: Python<'_>,
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
    let Ok(kwargs) = kwargs_any.cast::<pyo3::types::PyDict>() else {
        return Ok(());
    };
    let class_name_obj = cls.getattr("__name__")?;
    let class_name = class_name_obj
        .cast::<pyo3::types::PyString>()?
        .to_str()?
        .to_owned();

    if let Some(builder) = MIDDLEWARE_REGISTRY.get(&class_name) {
        builder.parse_kwargs(kwargs, container)?;
        container.record_layer(&class_name);
    } else {
        let py_middleware = PyMiddleware::new(_py, cls.clone().unbind());
        container.py_middlewares.push(Arc::new(py_middleware));
    }

    Ok(())
}
fn cached_method_router(method: HttpMethod, cached: Arc<CachedResponse>) -> MethodRouter {
    match_method_router!(method, {
        let cached = cached;
        move || {
            let cached = cached.clone();
            async move { cached.to_response() }
        }
    })
}

fn no_request_method_router(
    method: HttpMethod,
    handler: Arc<crate::routing::types::RouteHandler>,
    state: AppState,
) -> MethodRouter {
    if matches!(handler.execution.execution_mode, ExecutionMode::SyncNoArgs)
        && !state.sync_to_threadpool
    {
        return sync_no_request_method_router(method, handler);
    }

    match_method_router!(method, {
        let handler = handler;
        let state = state;
        move || {
            let handler = handler.clone();
            let state = state.clone();
            async move {
                let async_loop = state.pick_loop();
                py_bridge::scoped_request_loop(async_loop.clone(), async move {
                    run_py_handler_no_request(async_loop, state.sync_to_threadpool, handler).await
                })
                .await
            }
        }
    })
}

fn sync_no_request_method_router(
    method: HttpMethod,
    handler: Arc<crate::routing::types::RouteHandler>,
) -> MethodRouter {
    let use_json_fast_path = handler.response.response_model.is_none()
        && handler.response.response_class.is_none()
        && matches!(
            handler.response.response_type,
            crate::types::response::ResponseType::Json
        );

    match_method_router!(method, {
        let handler = handler;
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

fn precompute_const_response(
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

pub(crate) fn group_and_register_routes<F>(
    mut app: Router,
    routes: &[RouteEntry],
    mut builder: F,
) -> Router
where
    F: FnMut(&RouteEntry) -> Option<(&str, MethodRouter)>,
{
    let mut grouped_routes: AHashMap<&str, MethodRouter> = AHashMap::new();
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
        axum::http::Method::GET => "GET",
        axum::http::Method::POST => "POST",
        axum::http::Method::PUT => "PUT",
        axum::http::Method::DELETE => "DELETE",
        axum::http::Method::PATCH => "PATCH",
        axum::http::Method::OPTIONS => "OPTIONS",
        axum::http::Method::HEAD => "HEAD",
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
        other => metrics::SharedString::from(other.to_string()),
    };

    let path: metrics::SharedString = match response
        .extensions()
        .get::<crate::routing::router::RoutePattern>()
    {
        Some(pattern) => metrics::SharedString::from(pattern.0.clone()),
        None if response.status() == StatusCode::NOT_FOUND => {
            metrics::SharedString::from("unmatched")
        }
        None => metrics::SharedString::from(raw_uri.path().to_owned()),
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
