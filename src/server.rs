use axum::{
    extract::{ConnectInfo, Extension, Request},
    http::StatusCode,
    middleware::{self as axum_middleware, Next},
    response::{Html, IntoResponse},
    routing::*,
    Json, Router,
};
use once_cell::sync::Lazy;
use pyo3::exceptions::{PyRuntimeError, PyTypeError};
use pyo3::prelude::*;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{error, info, Level};

// middleware Libraries
use tower_http::compression::{predicate::SizeAbove, CompressionLayer};
use tower_sessions::cookie::Key;
use tower_sessions::{Expiry, MemoryStore, SessionManagerLayer};

// internal Imports
use crate::app::FastrAPI;
use crate::core::AppState;
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

struct EnteredLifespan {
    manager: Py<PyAny>,
    event_loop: Py<PyAny>,
}

pub fn serve(
    py: Python<'_>,
    host: Option<String>,
    port: Option<u16>,
    app: Py<FastrAPI>,
) -> PyResult<()> {
    tracing_subscriber::fmt()
        .with_max_level(Level::DEBUG)
        .with_target(false)
        .try_init()
        .ok();

    let host = host.unwrap_or_else(|| "127.0.0.1".to_string());
    let port = port.unwrap_or(8000);
    let rt_handle = PYTHON_RUNTIME.handle().clone();
    let app_state = AppState { rt_handle };

    let app_bound = app.bind(py);
    let app_config = app_bound.borrow();

    let docs_url = app_config.docs_url.clone();
    let openapi_url = app_config.openapi_url.clone();
    let docs_url_for_log = docs_url.clone();
    let on_startup = app_config
        .on_startup
        .as_ref()
        .map(|handler| handler.clone_ref(py));
    let on_shutdown = app_config
        .on_shutdown
        .as_ref()
        .map(|handler| handler.clone_ref(py));
    let lifespan = app_config
        .lifespan
        .as_ref()
        .map(|handler| handler.clone_ref(py));
    let app = app.clone_ref(py);

    let router = build_router(py, app_state.clone(), docs_url, openapi_url, &app_config);
    drop(app_config);

    let server_thread = std::thread::spawn(move || {
        let entered_lifespan = match run_startup_phase(app, lifespan, on_startup) {
            Ok(entered) => entered,
            Err(err) => {
                log_python_error("startup failed", err);
                return;
            }
        };

        let addr = format!("{}:{}", host, port);
        let server_result = PYTHON_RUNTIME.block_on(async move {
            let listener = TcpListener::bind(&addr)
                .await
                .map_err(|err| err.to_string())?;

            info!("🚀 FastrAPI running at http://{}", addr);
            if let Some(docs) = &docs_url_for_log {
                info!("📚 Swagger UI at http://{}{}", addr, docs);
            }

            let server = axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            );

            server
                .with_graceful_shutdown(async {
                    tokio::signal::ctrl_c()
                        .await
                        .expect("Failed to install Ctrl+C handler");

                    info!("Shutting down...");
                })
                .await
                .map_err(|err| err.to_string())
        });

        if let Err(err) = server_result {
            error!("Server error: {}", err);
        }

        if let Err(err) = run_shutdown_phase(entered_lifespan, on_shutdown) {
            log_python_error("shutdown failed", err);
        }
    });

    py.detach(move || server_thread.join())
        .map_err(|_| PyRuntimeError::new_err("Server thread panicked"))?;

    Ok(())
}

fn run_startup_phase(
    app: Py<FastrAPI>,
    lifespan: Option<Py<PyAny>>,
    on_startup: Option<Py<PyAny>>,
) -> PyResult<Option<EnteredLifespan>> {
    if let Some(lifespan_handler) = lifespan {
        return enter_lifespan(app, lifespan_handler).map(Some);
    }

    if let Some(startup_handlers) = on_startup {
        run_lifecycle_handlers(startup_handlers)?;
    }

    Ok(None)
}

fn run_shutdown_phase(
    entered_lifespan: Option<EnteredLifespan>,
    on_shutdown: Option<Py<PyAny>>,
) -> PyResult<()> {
    if let Some(entered) = entered_lifespan {
        return exit_lifespan(entered);
    }

    if let Some(shutdown_handlers) = on_shutdown {
        run_lifecycle_handlers(shutdown_handlers)?;
    }

    Ok(())
}

fn run_lifecycle_handlers(handlers: Py<PyAny>) -> PyResult<()> {
    for handler in extract_lifecycle_handlers(&handlers)? {
        run_lifecycle_handler(handler)?;
    }

    Ok(())
}

fn extract_lifecycle_handlers(handlers: &Py<PyAny>) -> PyResult<Vec<Py<PyAny>>> {
    Python::attach(|py| {
        let handlers_bound = handlers.bind(py);
        let builtins = py.import("builtins")?;

        if builtins
            .call_method1("callable", (handlers_bound,))?
            .extract::<bool>()?
        {
            return Ok(vec![handlers.clone_ref(py)]);
        }

        let mut extracted = Vec::new();
        for item in handlers_bound.try_iter()? {
            let handler = item?.unbind();
            if !builtins
                .call_method1("callable", (handler.bind(py),))?
                .extract::<bool>()?
            {
                return Err(PyTypeError::new_err(
                    "Lifecycle handlers must be callables or iterables of callables",
                ));
            }
            extracted.push(handler);
        }

        Ok(extracted)
    })
}

fn run_lifecycle_handler(handler: Py<PyAny>) -> PyResult<()> {
    let is_async = Python::attach(|py| -> PyResult<bool> {
        py.import("inspect")?
            .call_method1("iscoroutinefunction", (handler.bind(py),))?
            .extract()
    })?;

    if is_async {
        Python::attach(|py| -> PyResult<()> {
            let coroutine = handler.bind(py).call0()?;
            run_awaitable_in_new_loop(py, coroutine)
        })?;
    } else {
        Python::attach(|py| -> PyResult<()> {
            handler.bind(py).call0()?;
            Ok(())
        })?;
    }

    Ok(())
}

fn enter_lifespan(app: Py<FastrAPI>, lifespan: Py<PyAny>) -> PyResult<EnteredLifespan> {
    Python::attach(|py| -> PyResult<EnteredLifespan> {
        let event_loop = create_event_loop(py)?;
        let manager = lifespan.bind(py).call1((app.clone_ref(py),))?;
        let awaitable = manager.call_method0("__aenter__")?;

        if let Err(err) = run_awaitable_in_loop(py, event_loop.bind(py), awaitable) {
            shutdown_async_generators(event_loop.bind(py));
            close_event_loop(py, event_loop.bind(py));
            return Err(err);
        }

        Ok(EnteredLifespan {
            manager: manager.unbind(),
            event_loop,
        })
    })
}

fn exit_lifespan(entered_lifespan: EnteredLifespan) -> PyResult<()> {
    Python::attach(|py| -> PyResult<()> {
        let event_loop = entered_lifespan.event_loop.bind(py);
        let awaitable = entered_lifespan
            .manager
            .bind(py)
            .call_method1("__aexit__", (py.None(), py.None(), py.None()))?;
        let result = run_awaitable_in_loop(py, event_loop, awaitable);
        shutdown_async_generators(event_loop);
        close_event_loop(py, event_loop);
        result
    })?;

    Ok(())
}

fn create_event_loop(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let asyncio = py.import("asyncio")?;
    let event_loop = asyncio.call_method0("new_event_loop")?;
    asyncio.call_method1("set_event_loop", (&event_loop,))?;
    Ok(event_loop.unbind())
}

fn run_awaitable_in_new_loop(py: Python<'_>, awaitable: Bound<'_, PyAny>) -> PyResult<()> {
    let event_loop = create_event_loop(py)?;
    let result = run_awaitable_in_loop(py, event_loop.bind(py), awaitable);
    shutdown_async_generators(event_loop.bind(py));
    close_event_loop(py, event_loop.bind(py));
    result
}

fn run_awaitable_in_loop(
    py: Python<'_>,
    event_loop: &Bound<'_, PyAny>,
    awaitable: Bound<'_, PyAny>,
) -> PyResult<()> {
    let asyncio = py.import("asyncio")?;
    asyncio.call_method1("set_event_loop", (event_loop,))?;
    event_loop.call_method1("run_until_complete", (awaitable,))?;

    Ok(())
}

fn shutdown_async_generators(event_loop: &Bound<'_, PyAny>) {
    if let Ok(shutdown_asyncgens) = event_loop.call_method0("shutdown_asyncgens") {
        let _ = event_loop.call_method1("run_until_complete", (shutdown_asyncgens,));
    }
}

fn close_event_loop(py: Python<'_>, event_loop: &Bound<'_, PyAny>) {
    if let Ok(asyncio) = py.import("asyncio") {
        let _ = asyncio.call_method1("set_event_loop", (py.None(),));
    }

    let _ = event_loop.call_method0("close");
}

fn log_python_error(context: &str, err: PyErr) {
    error!("{}: {}", context, err);
    Python::attach(|py| err.print(py));
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
