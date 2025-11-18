use axum::extract::{ConnectInfo, Extension};
use axum::{middleware as axum_middleware, response::Html, routing::*, Json, Router};
use once_cell::sync::Lazy;
use pyo3::prelude::*;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tracing::{info, Level};

use crate::app::FastrAPI;
use crate::cors::build_cors_layer;
use crate::middlewares::{header_middleware, logging_middleware};
use crate::openapi::build_openapi_spec;
use crate::py_handlers::{run_py_handler_no_args, run_py_handler_with_args};
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

    // Pass the entire app config to build_router
    let router = build_router(py, app_state.clone(), docs_url, openapi_url, app);

    py.detach(move || {
        PYTHON_RUNTIME.block_on(async move {
            let addr = format!("{}:{}", host, port);
            let listener = TcpListener::bind(&addr).await.expect("Failed to bind");

            info!("FastrAPI running at http://{}", addr);
            if let Some(docs) = &docs_url_for_log {
                info!("Swagger UI at http://{}{}", addr, docs);
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
    app_config: &FastrAPI, // Received the config here
) -> Router {
    let mut app = Router::new();

    // route registering
    for entry in ROUTES.iter() {
        let route_key: Arc<str> = entry.key().clone().into();
        let parts: Vec<&str> = route_key.splitn(2, ' ').collect();

        if parts.len() != 2 {
            continue;
        }

        let method = parts[0];
        let path = parts[1].to_string();

        app = register_route(app, method, path, Arc::clone(&route_key), app_state.clone());
    }

    // OpenAPI endpoints
    let openapi_spec = build_openapi_spec(py, &ROUTES);
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

    if let Some(config) = &app_config.cors_config {
        match build_cors_layer(config) {
            Ok(cors_layer) => {
                app = app.layer(cors_layer);
                info!("CorsLayer applied successfully.");
            }
            Err(e) => {
                eprintln!("Error building CorsLayer: {:?}", e);
            }
        }
    }

    // to do: add built-in middlewares here
    // app = app
    //     .layer(axum_middleware::from_fn(header_middleware))
    //     .layer(axum_middleware::from_fn(logging_middleware));

    if !MIDDLEWARES.is_empty() {
        for entry in MIDDLEWARES.iter() {
            let middleware = entry.value().clone();
            app = app.layer(axum_middleware::from_fn(move |req, next| {
                let middleware = middleware.clone();
                async move { crate::middlewares::execute_py_middleware(middleware, req, next).await }
            }));
        }
    }

    app.layer(Extension(app_state))
}

fn register_route(
    app: Router,
    method: &str,
    path: String,
    route_key: Arc<str>,
    state: AppState,
) -> Router {
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
