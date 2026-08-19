use std::sync::Arc;

use axum::{
    Router,
    extract::Request,
    http::StatusCode,
    middleware::{self as axum_middleware, Next},
    response::IntoResponse,
};
use pyo3::{Py, PyAny};
use tower_http::{
    compression::{CompressionLayer, predicate::SizeAbove},
    normalize_path::NormalizePathLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    set_header::SetResponseHeaderLayer,
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use tower_sessions::{Expiry, SessionManagerLayer, cookie::Key};
use tracing::info;

use crate::engine::types::FastrAPI;
use crate::http::middleware::{
    DeclaredLayer, MiddlewareContainer, PyMiddleware, build_cors_layer, execute_py_middlewares,
};

pub(crate) fn build_stack(
    app: Router,
    app_config: &FastrAPI,
    container: &MiddlewareContainer,
    async_loop: Arc<Py<PyAny>>,
) -> Router {
    let app = if container.order.is_empty() {
        apply_legacy(app, container, async_loop)
    } else {
        apply_declared(app, container, async_loop)
    };

    apply_config_layers(app, app_config)
}

fn apply_declared(
    mut app: Router,
    container: &MiddlewareContainer,
    async_loop: Arc<Py<PyAny>>,
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
            DeclaredLayer::Custom(mw) => custom_layer(app, mw.clone(), async_loop.clone()),
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

fn session_layer(app: Router, container: &MiddlewareContainer) -> Router {
    let Some(config) = &container.session else {
        return app;
    };

    let key = Key::from(config.secret_key.as_bytes());
    let store = tower_sessions::MemoryStore::default();

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

fn trusted_host_layer(app: Router, container: &MiddlewareContainer) -> Router {
    let Some(config) = &container.trusted_host else {
        return app;
    };

    if config.allowed_hosts.iter().any(|host| host == "*") {
        return app;
    }

    use std::collections::HashSet;
    let allowed: Arc<HashSet<String>> = Arc::new(config.allowed_hosts.iter().cloned().collect());
    let redirect = config.www_redirect;

    app.layer(axum_middleware::from_fn(move |req: Request, next: Next| {
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
                let root = host_header.strip_prefix("www.").unwrap_or(host_header);
                if allowed.contains(root) {
                    return (StatusCode::MOVED_PERMANENTLY, "Redirecting...").into_response();
                }
            }

            (StatusCode::BAD_REQUEST, "Invalid Host Header").into_response()
        }
    }))
}

fn https_redirect_layer(app: Router) -> Router {
    app.layer(axum_middleware::from_fn(
        move |req: Request, next: Next| async move {
            let uri = req.uri().clone();
            let headers = req.headers().clone();

            let is_https = uri
                .scheme()
                .is_some_and(|s| s == &axum::http::uri::Scheme::HTTPS)
                || headers
                    .get("X-Forwarded-Proto")
                    .map(|v| v == "https")
                    .unwrap_or(false);

            if is_https {
                return next.run(req).await;
            }

            let mut parts = uri.into_parts();
            parts.scheme = Some(axum::http::uri::Scheme::HTTPS);
            if let Some(host) = headers.get("host").and_then(|h| h.to_str().ok()) {
                parts.authority = host.parse().ok();
            }
            if let Ok(new_uri) = axum::http::Uri::from_parts(parts) {
                return (
                    StatusCode::TEMPORARY_REDIRECT,
                    [(axum::http::header::LOCATION, new_uri.to_string())],
                    "Redirecting...",
                )
                    .into_response();
            }

            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        },
    ))
}

/// Decorator-style context middlewares (`@app.middleware("http")`).
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

    let py_mws = Arc::new(container.py_middlewares.clone());
    app.layer(axum_middleware::from_fn(move |req, next| {
        let py_mws = py_mws.clone();
        let loop_ = async_loop.clone();
        async move { execute_py_middlewares(py_mws, req, next, loop_).await }
    }))
}

fn custom_layer(app: Router, mw: Arc<PyMiddleware>, async_loop: Arc<Py<PyAny>>) -> Router {
    app.layer(axum_middleware::from_fn(move |req, next| {
        let mw = mw.clone();
        let loop_ = async_loop.clone();
        async move { execute_py_middlewares(Arc::new(vec![mw]), req, next, loop_).await }
    }))
}
