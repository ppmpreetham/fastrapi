//! The construction of the middleware stack of the server.
//! It adds the built-in middleware layers, such as CORS, GZip, session, trusted host and HTTPS redirect.
//! It also adds the Python, ASGI and decorator middleware.

use fastrapi_runtime::py_bridge::EventLoop;
use std::sync::Arc;

use axum::Router;
use axum::extract::Request;
use axum::http::uri::{Authority, PathAndQuery, Scheme, Uri};
use axum::http::{HeaderName, StatusCode, header};
use axum::middleware::{self as axum_middleware, Next};
use axum::response::{IntoResponse, Response};
use pyo3::prelude::PyResult;
use pyo3::types::{PyAnyMethods as _, PyTuple, PyTupleMethods as _};
use pyo3::{Bound, Py, PyAny, Python};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use tracing::{error, info};

use crate::engine::config::FastrAPI;
use crate::engine::server::routes::{
    parse_header_name, parse_header_value, record_prometheus_metrics,
};
use fastrapi_http::middleware::PyMiddlewareKind;
use fastrapi_http::middleware::{
    DeclaredLayer, MiddlewareContainer, PreparedMiddlewares, PyMiddleware, SessionPolicy, asgi,
    compress, cors, execute_py_middlewares, session,
};
use std::time;
use tower_http::catch_panic::CatchPanicLayer;

pub(crate) fn build_stack(
    app: Router,
    app_config: &FastrAPI,
    container: &MiddlewareContainer,
    event_loop: EventLoop,
) -> Router {
    let max_body_size = app_config.max_body_size.unwrap_or(usize::MAX);
    let threadpool = app_config.sync_to_threadpool;
    let app = if container.order.is_empty() {
        apply_legacy(app, container)
    } else {
        apply_declared(app, container, event_loop, max_body_size, threadpool)
    };

    apply_config_layers(app, app_config)
}

fn apply_declared(
    mut app: Router,
    container: &MiddlewareContainer,
    event_loop: EventLoop,
    max_body_size: usize,
    threadpool: bool,
) -> Router {
    for layer in &container.order {
        app = match layer {
            DeclaredLayer::Session => session_layer(app, container),
            DeclaredLayer::GZip => gzip_layer(app, container),
            DeclaredLayer::Cors => cors_layer(app, container),
            DeclaredLayer::TrustedHost => trusted_host_layer(app, container),
            DeclaredLayer::HttpsRedirect => https_redirect_layer(app),
            DeclaredLayer::Custom(mw) => {
                if mw.kind == PyMiddlewareKind::Asgi {
                    asgi_layer(app, mw, event_loop, max_body_size)
                } else {
                    custom_layer(app, mw.clone(), event_loop, threadpool)
                }
            }
            DeclaredLayer::Decorator(mw) => decorator_layer(app, mw, event_loop, threadpool),
        };
    }
    app
}

fn apply_legacy(mut app: Router, container: &MiddlewareContainer) -> Router {
    if container.session.is_some() {
        info!("🔑 Layer: Sessions");
        app = session_layer(app, container);
    }

    if let Some(config) = &container.gzip {
        info!("🗜️ Layer: GZip (min: {} bytes)", config.minimum_size);
        app = gzip_layer(app, container);
    }

    if let Some(_config) = &container.https_redirect {
        info!("🔗 Layer: HTTPSRedirect");
        app = https_redirect_layer(app);
    }

    if container.trusted_host.is_some() {
        info!("🛡️ Layer: TrustedHost");
        app = trusted_host_layer(app, container);
    }

    if container.cors.is_some() {
        info!("Layer: CORS");
        app = cors_layer(app, container);
    }

    app
}

fn apply_config_layers(mut app: Router, app_config: &FastrAPI) -> Router {
    if app_config.prometheus_config.is_some() {
        app = app.layer(axum_middleware::from_fn(record_prometheus_metrics));
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
            time::Duration::from_secs(seconds),
        ));
    }

    if app_config.catch_panics {
        app = app.layer(CatchPanicLayer::new());
    }

    app
}

fn session_layer(app: Router, container: &MiddlewareContainer) -> Router {
    match &container.session {
        Some(config) => app.layer(axum_middleware::from_fn_with_state(
            Arc::new(SessionPolicy::new(config)),
            session,
        )),
        None => app,
    }
}

fn gzip_layer(app: Router, container: &MiddlewareContainer) -> Router {
    match &container.gzip {
        Some(config) => app.layer(axum_middleware::from_fn_with_state(
            Arc::new(config.clone()),
            compress,
        )),
        None => app,
    }
}

fn cors_layer(app: Router, container: &MiddlewareContainer) -> Router {
    match &container.cors {
        Some(policy) => Router::new()
            .fallback_service(app)
            .layer(axum_middleware::from_fn_with_state(Arc::clone(policy), cors)),
        None => app,
    }
}

fn host_matches(allowed: &[String], host: &str) -> bool {
    allowed
        .iter()
        .any(|pattern| match pattern.strip_prefix('*') {
            Some(suffix) => host.ends_with(suffix),
            None => pattern == host,
        })
}

fn redirect_to(req: &Request, scheme: Scheme, netloc: &str) -> Response {
    let uri = req.uri();
    let target = Uri::builder()
        .scheme(scheme)
        .authority(netloc)
        .path_and_query(uri.path_and_query().map_or("/", PathAndQuery::as_str))
        .build();

    match target {
        Ok(target) => (
            StatusCode::TEMPORARY_REDIRECT,
            [(header::LOCATION, target.to_string())],
        )
            .into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}

fn trusted_host_layer(app: Router, container: &MiddlewareContainer) -> Router {
    let Some(config) = &container.trusted_host else {
        return app;
    };

    if config.allowed_hosts.iter().any(|host| host == "*") {
        return app;
    }

    let allowed: Arc<[String]> = Arc::from(config.allowed_hosts.as_slice());
    let redirect = config.www_redirect;

    app.layer(axum_middleware::from_fn(move |req: Request, next: Next| {
        let allowed = allowed.clone();
        async move {
            let authority = req
                .headers()
                .get(header::HOST)
                .and_then(|host| host.to_str().ok())
                .unwrap_or_default();
            let host = fastrapi_core::path::host_without_port(authority);

            if host_matches(&allowed, host) {
                return next.run(req).await;
            }

            if redirect
                && allowed
                    .iter()
                    .any(|pattern| pattern.strip_prefix("www.") == Some(host))
            {
                let scheme = req.uri().scheme().cloned().unwrap_or(Scheme::HTTP);
                return redirect_to(&req, scheme, &format!("www.{authority}"));
            }

            (StatusCode::BAD_REQUEST, "Invalid host header").into_response()
        }
    }))
}

fn drop_default_port(authority: &Authority) -> String {
    match authority.port_u16() {
        Some(80 | 443) => authority.host().to_owned(),
        _ => authority.to_string(),
    }
}

fn https_redirect_layer(app: Router) -> Router {
    app.layer(axum_middleware::from_fn(
        move |req: Request, next: Next| async move {
            if req.uri().scheme() == Some(&Scheme::HTTPS) {
                return next.run(req).await;
            }

            let Some(authority) = req
                .headers()
                .get(header::HOST)
                .and_then(|host| host.to_str().ok())
                .and_then(|host| host.parse::<Authority>().ok())
            else {
                return StatusCode::BAD_REQUEST.into_response();
            };

            redirect_to(&req, Scheme::HTTPS, &drop_default_port(&authority))
        },
    ))
}

fn decorator_layer(
    app: Router,
    middleware: &Arc<PyMiddleware>,
    event_loop: EventLoop,
    sync_to_threadpool: bool,
) -> Router {
    let prepared = Arc::new(PreparedMiddlewares::new(
        std::slice::from_ref(middleware),
        sync_to_threadpool,
    ));
    app.layer(axum_middleware::from_fn(move |req, next| {
        let prepared = prepared.clone();
        async move { execute_py_middlewares(prepared, req, next, event_loop).await }
    }))
}

fn asgi_layer(
    app: Router,
    mw: &PyMiddleware,
    event_loop: EventLoop,
    max_body_size: usize,
) -> Router {
    let instance = Python::attach(|py| -> PyResult<Py<PyAny>> {
        let downstream = Py::new(py, asgi::PyScopeDownstream)?.into_any();
        let cls = mw.func.bind(py);
        let mut args = vec![downstream.bind(py).clone()];
        args.extend(mw.init_args.bind(py).iter());
        let args = PyTuple::new(py, args)?;
        cls.call(args, mw.init_kwargs.as_ref().map(|kwargs| kwargs.bind(py)))
            .map(Bound::unbind)
    });

    match instance {
        Ok(instance) => {
            info!("Applying raw ASGI middleware");
            let instance = Arc::new(instance);
            app.layer(axum_middleware::from_fn(move |req, next| {
                let instance = instance.clone();
                let event_loop = event_loop;
                async move {
                    asgi::run_asgi_request(instance, req, next, event_loop, max_body_size).await
                }
            }))
        }
        Err(err) => {
            Python::attach(|py| err.print(py));
            error!("Failed to instantiate ASGI middleware, skipping it");
            app
        }
    }
}

fn custom_layer(
    app: Router,
    mw: Arc<PyMiddleware>,
    event_loop: EventLoop,
    sync_to_threadpool: bool,
) -> Router {
    let prepared = Arc::new(PreparedMiddlewares::new(&[mw], sync_to_threadpool));
    app.layer(axum_middleware::from_fn(move |req, next| {
        let prepared = prepared.clone();
        let loop_ = event_loop;
        async move { execute_py_middlewares(prepared, req, next, loop_).await }
    }))
}
