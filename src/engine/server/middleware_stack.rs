use std::sync::Arc;

use axum::{
    Router,
    extract::Request,
    http::{
        StatusCode, header,
        uri::{Authority, PathAndQuery, Scheme, Uri},
    },
    middleware::{self as axum_middleware, Next},
    response::{IntoResponse, Response},
};
use pyo3::{Py, PyAny, Python, prelude::PyResult, types::PyAnyMethods as _};
use sha2::{Digest, Sha256};
use tower_http::{
    compression::{CompressionLayer, predicate::SizeAbove},
    normalize_path::NormalizePathLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    set_header::SetResponseHeaderLayer,
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use tower_sessions::{
    Expiry, SessionManagerLayer,
    cookie::{Key, SameSite, time::Duration},
};
use tracing::info;

use tracing::error;

use crate::engine::types::FastrAPI;
use crate::http::middleware::{
    DeclaredLayer, MiddlewareContainer, PreparedMiddlewares, PyMiddleware, asgi, build_cors_layer,
    execute_py_middlewares,
};

pub(crate) fn build_stack(
    app: Router,
    app_config: &FastrAPI,
    container: &MiddlewareContainer,
    async_loop: Arc<Py<PyAny>>,
) -> Router {
    let max_body_size = app_config.max_body_size.unwrap_or(usize::MAX);
    let app = if container.order.is_empty() {
        apply_legacy(app, container, async_loop)
    } else {
        apply_declared(app, container, async_loop, max_body_size)
    };

    apply_config_layers(app, app_config)
}

fn apply_declared(
    mut app: Router,
    container: &MiddlewareContainer,
    async_loop: Arc<Py<PyAny>>,
    max_body_size: usize,
) -> Router {
    // axum's `.layer()` makes each successive call OUTERMOST, so applying
    // in declaration order yields Starlette semantics: the last-declared
    // middleware runs first.
    for layer in container.order.iter() {
        app = match layer {
            DeclaredLayer::Session => session_layer(app, container),
            DeclaredLayer::GZip => gzip_layer(app, container),
            DeclaredLayer::Cors => cors_layer(app, container),
            DeclaredLayer::TrustedHost => trusted_host_layer(app, container),
            DeclaredLayer::HttpsRedirect => https_redirect_layer(app),
            DeclaredLayer::Custom(mw) => {
                if mw.kind == crate::http::middleware::PyMiddlewareKind::Asgi {
                    asgi_layer(app, mw.clone(), async_loop.clone(), max_body_size)
                } else {
                    custom_layer(app, mw.clone(), async_loop.clone())
                }
            }
        };
    }
    // Decorator-style middlewares stay innermost.
    decorator_layer(app, container, async_loop)
}

fn apply_legacy(
    mut app: Router,
    container: &MiddlewareContainer,
    async_loop: Arc<Py<PyAny>>,
) -> Router {
    if container.session.is_some() {
        info!("🔑 Layer: Sessions");
        app = session_layer(app, container);
    }

    if let Some(config) = &container.gzip {
        info!("🗜️ Layer: GZip (min: {} bytes)", config.minimum_size);
        app = gzip_layer(app, container);
    }

    if !container.py_middlewares.is_empty() {
        app = decorator_layer(app, container, async_loop);
    }

    if let Some(_config) = &container.https_redirect {
        info!("🔗 Layer: HTTPSRedirect");
        app = https_redirect_layer(app);
    }

    if container.trusted_host.is_some() {
        info!("🛡️ Layer: TrustedHost");
        app = trusted_host_layer(app, container);
    }

    if let Some(config) = &container.cors {
        info!("Layer: CORS");
        match build_cors_layer(config) {
            Ok(layer) => app = app.layer(layer),
            Err(e) => eprintln!("Error building CORS layer: {:?}", e),
        }
    }

    app
}

fn apply_config_layers(mut app: Router, app_config: &FastrAPI) -> Router {
    if app_config.prometheus_config.is_some() {
        app = app.layer(axum_middleware::from_fn(
            crate::engine::server::routes::record_prometheus_metrics,
        ));
    }

    if app_config.trace_requests {
        app = app.layer(TraceLayer::new_for_http());
    }

    if let Some(header_name) = app_config
        .request_id_header
        .as_deref()
        .and_then(crate::engine::server::routes::parse_header_name)
    {
        app = app
            .layer(SetRequestIdLayer::new(header_name.clone(), MakeRequestUuid))
            .layer(PropagateRequestIdLayer::new(header_name));
    }

    if let Some(value) = app_config
        .powered_by_header
        .as_deref()
        .and_then(crate::engine::server::routes::parse_header_value)
    {
        app = app.layer(SetResponseHeaderLayer::if_not_present(
            axum::http::HeaderName::from_static("x-powered-by"),
            value,
        ));
    }

    if let Some(seconds) = app_config.request_timeout {
        app = app.layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            std::time::Duration::from_secs(seconds),
        ));
    }

    if app_config.catch_panics {
        app = app.layer(tower_http::catch_panic::CatchPanicLayer::new());
    }

    if app_config.redirect_slashes {
        app = app.layer(NormalizePathLayer::trim_trailing_slash());
    }

    app
}

fn session_key(secret: &str) -> Key {
    Key::derive_from(&Sha256::digest(secret.as_bytes()))
}

static X_FORWARDED_PROTO: header::HeaderName = header::HeaderName::from_static("x-forwarded-proto");

fn same_site(value: &str) -> SameSite {
    match value {
        "strict" => SameSite::Strict,
        "none" => SameSite::None,
        _ => SameSite::Lax,
    }
}

fn session_layer(app: Router, container: &MiddlewareContainer) -> Router {
    let Some(config) = &container.session else {
        return app;
    };

    let layer = SessionManagerLayer::new(tower_sessions::MemoryStore::default())
        .with_signed(session_key(&config.secret_key))
        .with_name(config.session_cookie.clone())
        .with_path(config.path.clone())
        .with_secure(config.https_only)
        .with_http_only(true)
        .with_same_site(same_site(&config.same_site));

    let layer = match &config.domain {
        Some(domain) => layer.with_domain(domain.clone()),
        None => layer,
    };
    let layer = match config.max_age {
        Some(max_age) => layer.with_expiry(Expiry::OnInactivity(Duration::seconds(max_age))),
        None => layer,
    };

    app.layer(layer)
}

fn gzip_layer(app: Router, container: &MiddlewareContainer) -> Router {
    match &container.gzip {
        Some(config) => app.layer(
            CompressionLayer::new().compress_when(SizeAbove::new(config.minimum_size as u16)),
        ),
        None => app,
    }
}

fn cors_layer(app: Router, container: &MiddlewareContainer) -> Router {
    match &container.cors {
        Some(config) => match build_cors_layer(config) {
            Ok(layer) => app.layer(layer),
            Err(e) => {
                eprintln!("Error building CORS layer: {e:?}");
                app
            }
        },
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

fn redirect_to(req: &Request, netloc: &str) -> Response {
    let uri = req.uri();
    let target = Uri::builder()
        .scheme(uri.scheme().cloned().unwrap_or(Scheme::HTTP))
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
            let host = authority.split(':').next().unwrap_or_default();

            if host_matches(&allowed, host) {
                return next.run(req).await;
            }

            if redirect && host_matches(&allowed, &format!("www.{host}")) {
                return redirect_to(&req, &format!("www.{authority}"));
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
            let is_https = req.uri().scheme() == Some(&Scheme::HTTPS)
                || req
                    .headers()
                    .get(&X_FORWARDED_PROTO)
                    .is_some_and(|proto| proto == "https");

            if is_https {
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

            redirect_to(&req, &drop_default_port(&authority))
        },
    ))
}

fn decorator_layer(
    app: Router,
    container: &MiddlewareContainer,
    async_loop: Arc<Py<PyAny>>,
) -> Router {
    if container.py_middlewares.is_empty() {
        return app;
    }

    info!(
        "Applying {} custom Python middleware(s)",
        container.py_middlewares.len()
    );

    let py_mws = Arc::new(PreparedMiddlewares::new(Arc::new(
        container.py_middlewares.clone(),
    )));
    app.layer(axum_middleware::from_fn(move |req, next| {
        let py_mws = py_mws.clone();
        let loop_ = async_loop.clone();
        async move { execute_py_middlewares(py_mws, req, next, loop_).await }
    }))
}

fn asgi_layer(
    app: Router,
    mw: Arc<PyMiddleware>,
    async_loop: Arc<Py<PyAny>>,
    max_body_size: usize,
) -> Router {
    let instance = Python::attach(|py| -> PyResult<Py<PyAny>> {
        let downstream = Py::new(py, asgi::PyScopeDownstream)?.into_any();
        let cls = mw.func.bind(py);
        match &mw.init_kwargs {
            Some(kwargs) => cls.call((downstream,), Some(kwargs.bind(py))),
            None => cls.call1((downstream,)),
        }
        .map(|obj| obj.unbind())
    });

    match instance {
        Ok(instance) => {
            info!("Applying raw ASGI middleware");
            let instance = std::sync::Arc::new(instance);
            app.layer(axum_middleware::from_fn(move |req, next| {
                let instance = instance.clone();
                let async_loop = async_loop.clone();
                async move {
                    asgi::run_asgi_request(instance, req, next, async_loop, max_body_size).await
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

fn custom_layer(app: Router, mw: Arc<PyMiddleware>, async_loop: Arc<Py<PyAny>>) -> Router {
    app.layer(axum_middleware::from_fn(move |req, next| {
        let mw = mw.clone();
        let loop_ = async_loop.clone();
        async move {
            let prepared = Arc::new(PreparedMiddlewares::new(Arc::new(vec![mw])));
            execute_py_middlewares(prepared, req, next, loop_).await
        }
    }))
}
