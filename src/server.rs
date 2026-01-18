use axum::{
    extract::{ConnectInfo, Extension, Request},
    http::StatusCode,
    middleware::{self as axum_middleware, Next},
    response::{Html, IntoResponse},
    routing::*,
    Json, Router,
};
use once_cell::sync::Lazy;
use pyo3::prelude::*;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{info, Level};

// middleware Libraries
use tower_http::compression::{predicate::SizeAbove, CompressionLayer};
use tower_sessions::cookie::Key;
use tower_sessions::{Expiry, MemoryStore, SessionManagerLayer};

// internal Imports
use crate::app::FastrAPI;
use crate::middlewares::build_cors_layer;
use crate::openapi::build_openapi_spec;
use crate::py_handlers::{run_py_handler_no_args, run_py_handler_with_args};
use crate::utils::local_guard;
use crate::websocket::{ws_handler, WEBSOCKET_ROUTES};
use crate::{MIDDLEWARES, ROUTES};

static PYTHON_RUNTIME: Lazy<tokio::runtime::Runtime> = Lazy::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(num_cpus::get().max(4).min(16))
        .thread_name("python-handler")
        .enable_all()
        .build()
        .expect("Failed to create Python runtime")
});

#[derive(Clone)]
pub struct AppState {
    pub rt_handle: tokio::runtime::Handle,
}

pub fn serve(py: Python, host: Option<String>, port: Option<u16>, app: &FastrAPI) -> PyResult<()> {
    tracing_subscriber::fmt()
        .with_max_level(Level::DEBUG)
        .with_target(false)
        .try_init()
        .ok();

    let host = host.unwrap_or_else(|| "127.0.0.1".to_string());
    let port = port.unwrap_or(8000);
    let rt_handle = PYTHON_RUNTIME.handle().clone();
    let app_state = AppState { rt_handle };

    let docs_url = app.docs_url.clone();
    let openapi_url = app.openapi_url.clone();
    let docs_url_for_log = docs_url.clone();

    let router = build_router(py, app_state.clone(), docs_url, openapi_url, app);

    py.detach(move || {
        PYTHON_RUNTIME.block_on(async move {
            let addr = format!("{}:{}", host, port);
            let listener = TcpListener::bind(&addr).await.expect("Failed to bind");

            info!("🚀 FastrAPI running at http://{}", addr);
            if let Some(docs) = &docs_url_for_log {
                info!("📚 Swagger UI at http://{}{}", addr, docs);
            }

            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("Server error");
        });
    });

    Ok(())
}

fn build_router(
    py: Python,
    app_state: AppState,
    docs_url: Option<String>,
    openapi_url: String,
    app_config: &FastrAPI,
) -> Router {
    let mut app = Router::new();

    let session_config = app_config.session_config.clone();
    let gzip_config = app_config.gzip_config.clone();
    let cors_config = app_config.cors_config.clone();
    let trusted_host_config = app_config.trusted_host_config.clone();

    // Route registration
    let guard = local_guard(&*ROUTES);
    for entry in ROUTES.iter(&guard) {
        let (route_key, _handler) = entry;
        let parts: Vec<&str> = route_key.splitn(2, ' ').collect();
        if parts.len() != 2 {
            continue;
        }
        let method = parts[0];
        let path = parts[1].to_string();
        app = register_route(
            app,
            method,
            path,
            route_key.as_str().into(),
            app_state.clone(),
        );
    }

    // Websockets
    let guard = crate::utils::local_guard(&*WEBSOCKET_ROUTES);
    for (key, _) in WEBSOCKET_ROUTES.iter(&guard) {
        let parts: Vec<&str> = key.splitn(2, ' ').collect();
        if parts.len() != 2 || parts[0] != "WS" {
            continue;
        }
        let path = parts[1].to_string();
        let route_key = Arc::new(key.clone());

        app = app.route(
            &path,
            get({
                let rt_handle = app_state.rt_handle.clone();
                move |ws| ws_handler(ws, Extension(route_key.clone()), Extension(rt_handle))
            }),
        );
    }

    // OpenAPI
    let openapi_spec = build_openapi_spec(py, &*ROUTES);
    let openapi_json = Arc::new(serde_json::to_value(&openapi_spec).unwrap());

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
        app = app.route(
            &docs,
            get(|| async { Html(include_str!("../static/swagger-ui.html")) }),
        );
    }

    // =========================== //
    // ==== LAYER APPLICATION ==== //
    // =========================== //

    // L1: Sessions
    if let Some(config) = session_config {
        info!("🍪 Layer: Sessions");
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
        info!("🗜️ Layer: GZip (min: {} bytes)", config.minimum_size);
        let predicate = SizeAbove::new(config.minimum_size as u16);
        app = app.layer(CompressionLayer::new().compress_when(predicate));
    }

    // L3: Python Middleware
    if !MIDDLEWARES.is_empty() {
        info!("Applying {} custom Python middleware(s)", MIDDLEWARES.len());

        let guard = local_guard(&*MIDDLEWARES);
        for (_key, middleware_ref) in MIDDLEWARES.iter(&guard) {
            let middleware = middleware_ref.clone();

            app = app.layer(axum_middleware::from_fn(move |req, next| {
            let middleware = middleware.clone();
            async move {
                crate::middlewares::execute_py_middleware(middleware, req, next).await
            }
        }));
        }
    }

    // L4: CORS
    if let Some(config) = cors_config {
        info!("🛡️ Layer: CORS");
        match build_cors_layer(&config) {
            Ok(layer) => {
                app = app.layer(layer);
            }
            Err(e) => eprintln!("Error building CORS layer: {:?}", e),
        }
    }

    // L5: Trusted Host
    if let Some(config) = trusted_host_config {
        info!("🔒 Layer: TrustedHost");
        let allowed = Arc::new(config.allowed_hosts);
        let redirect = config.www_redirect;

        app = app.layer(axum_middleware::from_fn(move |req: Request, next: Next| {
            let allowed = allowed.clone();
            async move {
                let host_header = req
                    .headers()
                    .get("host")
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or("");

                if allowed.contains(&"*".to_string()) || allowed.iter().any(|h| h == host_header) {
                    return next.run(req).await;
                }

                if redirect && host_header.starts_with("www.") {
                    let root = host_header.strip_prefix("www.").unwrap();
                    if allowed.iter().any(|h| h == root) {
                        return (axum::http::StatusCode::MOVED_PERMANENTLY, "Redirecting...")
                            .into_response();
                    }
                }

                (StatusCode::BAD_REQUEST, "Invalid Host Header").into_response()
            }
        }));
    }

    app.layer(Extension(app_state))
}

// Helper
fn register_route(
    app: Router,
    method: &str,
    path: String,
    route_key: Arc<str>,
    _state: AppState,
) -> Router {
    let _handler_key = Arc::clone(&route_key);
    match method {
        "GET" | "HEAD" | "OPTIONS" => {
            let route_key_clone = Arc::clone(&route_key);
            let handler =
                move |Extension(state): Extension<AppState>,
                      ConnectInfo(_addr): ConnectInfo<SocketAddr>| {
                    let route_key = Arc::clone(&route_key_clone);
                    async move { run_py_handler_no_args(state.rt_handle, route_key).await }
                };

            match method {
                "GET" => app.route(&path, get(handler)),
                "HEAD" => app.route(&path, head(handler)),
                "OPTIONS" => app.route(&path, options(handler)),
                _ => app,
            }
        }
        "POST" | "PUT" | "DELETE" | "PATCH" => {
            let route_key_clone = Arc::clone(&route_key);
            let handler = move |Extension(state): Extension<AppState>,
                                ConnectInfo(_addr): ConnectInfo<SocketAddr>,
                                Json(payload)| {
                let route_key = Arc::clone(&route_key_clone);
                async move { run_py_handler_with_args(state.rt_handle, route_key, payload).await }
            };

            match method {
                "POST" => app.route(&path, post(handler)),
                "PUT" => app.route(&path, put(handler)),
                "DELETE" => app.route(&path, delete(handler)),
                "PATCH" => app.route(&path, patch(handler)),
                _ => app,
            }
        }
        _ => app,
    }
}
