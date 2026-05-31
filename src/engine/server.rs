use super::types::FastrAPI;
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Extension, Request},
    http::StatusCode,
    middleware::{self as axum_middleware, Next},
    response::{Html, IntoResponse, Response},
    routing::*,
    serve::ListenerExt,
};
use pyo3::exceptions::{PyRuntimeError, PyTypeError};
use pyo3::prelude::*;
use smallvec::SmallVec;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{Level, error, info};

// middleware Libraries
use tower_http::compression::{CompressionLayer, predicate::SizeAbove};
use tower_sessions::cookie::Key;
use tower_sessions::{Expiry, MemoryStore, SessionManagerLayer};

// internal Imports
use crate::utils::openapi::build_openapi_spec;
use crate::utils::utils::local_guard;
use crate::{
    ffi::py_handlers::run_py_handler,
    routing::router::{FrozenRouter, FrozenRouterBuilder},
};
use crate::{
    globals::{MIDDLEWARES, PYTHON_RUNTIME},
    routing::types::HttpMethod,
};
use crate::{
    http::middleware::{
        CORSMiddleware, GZipMiddleware, SessionMiddleware, TrustedHostMiddleware, build_cors_layer,
        parse_cors_params, parse_gzip_params, parse_session_params, parse_trusted_host_params,
    },
    routing::router::RouteMatch,
};
use crate::{http::websocket::ws_handler, routing::types::PathParamRange};

#[derive(Clone)]
pub struct AppState {
    pub rt_handle: tokio::runtime::Handle,
}

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

    let host: String = host.unwrap_or_else(|| "127.0.0.1".to_string());
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
        let entered_lifespan =
            Python::attach(|py| run_startup_phase(py, app, lifespan, on_startup));

        let entered_lifespan = match entered_lifespan {
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

            let listener = listener.tap_io(|stream| {
                let _ = stream.set_nodelay(true);
            });

            let service = router.into_make_service_with_connect_info::<std::net::SocketAddr>();
            let server = axum::serve(listener, service);

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

        Python::attach(|py| {
            if let Err(err) = run_shutdown_phase(py, entered_lifespan, on_shutdown) {
                log_python_error("shutdown failed", err);
            }
        });
    });

    py.detach(move || server_thread.join())
        .map_err(|_| PyRuntimeError::new_err("Server thread panicked"))?;

    Ok(())
}

fn run_startup_phase(
    py: Python<'_>,
    app: Py<FastrAPI>,
    lifespan: Option<Py<PyAny>>,
    on_startup: Option<Py<PyAny>>,
) -> PyResult<Option<EnteredLifespan>> {
    if let Some(lifespan_handler) = lifespan {
        return enter_lifespan(py, app, lifespan_handler).map(Some); // Pass py here too
    }

    if let Some(startup_handlers) = on_startup {
        run_lifecycle_handlers(py, startup_handlers)?;
    }

    Ok(None)
}

fn run_shutdown_phase(
    py: Python<'_>, // <-- Add this
    entered_lifespan: Option<EnteredLifespan>,
    on_shutdown: Option<Py<PyAny>>,
) -> PyResult<()> {
    if let Some(entered) = entered_lifespan {
        return exit_lifespan(py, entered);
    }

    if let Some(shutdown_handlers) = on_shutdown {
        run_lifecycle_handlers(py, shutdown_handlers)?;
    }

    Ok(())
}
fn run_lifecycle_handlers(py: Python<'_>, handlers: Py<PyAny>) -> PyResult<()> {
    extract_lifecycle_handlers(py, &handlers)?
        .into_iter()
        .try_for_each(|handler| run_lifecycle_handler(py, handler))
}

fn extract_lifecycle_handlers<'py>(
    py: Python<'py>,
    handlers: &Py<PyAny>,
) -> PyResult<Vec<Py<PyAny>>> {
    let handlers_bound = handlers.bind(py);

    if handlers_bound.is_callable() {
        return Ok(vec![handlers.clone_ref(py)]);
    }

    handlers_bound
        .try_iter()?
        .map(|item| {
            let handler = item?;
            if !handler.is_callable() {
                return Err(PyTypeError::new_err(
                    "Lifecycle handlers must be callables or iterables of callables",
                ));
            }
            Ok(handler.into())
        })
        .collect::<PyResult<Vec<Py<PyAny>>>>()
}

fn run_lifecycle_handler(py: Python<'_>, handler: Py<PyAny>) -> PyResult<()> {
    let handler_bound = handler.bind(py);

    let is_async: bool = py
        .import("inspect")?
        .call_method1("iscoroutinefunction", (handler_bound,))?
        .is_truthy()?;

    if is_async {
        let coroutine = handler_bound.call0()?;
        run_awaitable_in_new_loop(py, coroutine)?;
    } else {
        handler_bound.call0()?;
    }

    Ok(())
}

fn enter_lifespan(
    py: Python<'_>,
    app: Py<FastrAPI>,
    lifespan: Py<PyAny>,
) -> PyResult<EnteredLifespan> {
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
}

fn exit_lifespan(py: Python<'_>, entered_lifespan: EnteredLifespan) -> PyResult<()> {
    let event_loop = entered_lifespan.event_loop.bind(py);

    let awaitable = entered_lifespan
        .manager
        .bind(py)
        .call_method1("__aexit__", (py.None(), py.None(), py.None()))?;

    let result = run_awaitable_in_loop(py, event_loop, awaitable);

    shutdown_async_generators(event_loop);
    close_event_loop(py, event_loop);

    result
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

async fn extract_payload(body: Body) -> Result<Option<serde_json::Value>, Response> {
    let body = to_bytes(body, usize::MAX)
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, "Failed to read request body").into_response())?;
    if body.is_empty() {
        return Ok(None);
    }
    let mut buf = body.to_vec();
    simd_json::serde::from_slice(&mut buf)
        .map(Some)
        .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "Invalid JSON body").into_response())
}

fn apply_declared_middleware(
    _py: Python<'_>,
    middleware_item: &Bound<'_, PyAny>,
    cors_config: &mut Option<CORSMiddleware>,
    trusted_host_config: &mut Option<TrustedHostMiddleware>,
    gzip_config: &mut Option<GZipMiddleware>,
    session_config: &mut Option<SessionMiddleware>,
) -> PyResult<()> {
    if let Ok(config) = middleware_item.extract::<PyRef<'_, CORSMiddleware>>() {
        *cors_config = Some(config.clone());
        return Ok(());
    }
    if let Ok(config) = middleware_item.extract::<PyRef<'_, TrustedHostMiddleware>>() {
        *trusted_host_config = Some(config.clone());
        return Ok(());
    }
    if let Ok(config) = middleware_item.extract::<PyRef<'_, GZipMiddleware>>() {
        *gzip_config = Some(config.clone());
        return Ok(());
    }
    if let Ok(config) = middleware_item.extract::<PyRef<'_, SessionMiddleware>>() {
        *session_config = Some(config.clone());
        return Ok(());
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

    match class_name.as_str() {
        "CORSMiddleware" => *cors_config = Some(parse_cors_params(kwargs)?),
        "TrustedHostMiddleware" => *trusted_host_config = Some(parse_trusted_host_params(kwargs)?),
        "GZipMiddleware" => *gzip_config = Some(parse_gzip_params(kwargs)?),
        "SessionMiddleware" => *session_config = Some(parse_session_params(kwargs)?),
        _ => {}
    }

    Ok(())
}

fn merge_declared_middlewares(
    py: Python<'_>,
    app_config: &FastrAPI,
    cors_config: &mut Option<CORSMiddleware>,
    trusted_host_config: &mut Option<TrustedHostMiddleware>,
    gzip_config: &mut Option<GZipMiddleware>,
    session_config: &mut Option<SessionMiddleware>,
) {
    let Some(middlewares) = &app_config.middleware else {
        return;
    };

    let middlewares = middlewares.bind(py);
    let Ok(iter) = middlewares.try_iter() else {
        return;
    };

    iter.flatten().for_each(|item| {
        if let Err(err) = apply_declared_middleware(
            py,
            &item,
            cors_config,
            trusted_host_config,
            gzip_config,
            session_config,
        ) {
            log_python_error("middleware setup failed", err);
        }
    });
}

fn build_router(
    py: Python,
    app_state: AppState,
    docs_url: Option<String>,
    openapi_url: String,
    app_config: &FastrAPI,
) -> Router {
    let mut app = Router::new();

    let mut session_config = app_config.session_config.clone();
    let mut gzip_config = app_config.gzip_config.clone();
    let mut cors_config = app_config.cors_config.clone();
    let mut trusted_host_config = app_config.trusted_host_config.clone();

    merge_declared_middlewares(
        py,
        app_config,
        &mut cors_config,
        &mut trusted_host_config,
        &mut gzip_config,
        &mut session_config,
    );

    let base_router = app_config.router.bind(py);
    let base_ref = base_router.borrow();
    base_ref.freeze(py);
    let flat = base_ref.flatten(py);

    let mut frozen_router_builder = FrozenRouterBuilder::new();
    flat.0.iter().for_each(|route| {
        frozen_router_builder.add_route(route.method, route.path.clone(), route.handler.clone());
    });

    let frozen_router = Arc::new(frozen_router_builder.build());

    app = flat.1.iter().fold(app, |current_app, ws| {
        let path = ws.path.clone();
        let handler = ws.handler.clone_ref(py);
        let rt_handle = app_state.rt_handle.clone();

        current_app.route(
            &path,
            axum::routing::get(move |ws_upgrade| {
                ws_handler(
                    ws_upgrade,
                    axum::Extension(handler.clone()),
                    axum::Extension(rt_handle.clone()),
                )
            }),
        )
    });

    let openapi_json = Arc::new(build_openapi_spec(
        py,
        &base_ref,
        &app_config.title,
        &app_config.version,
        &app_config.description,
    ));

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
            get(|| async { Html(include_str!("../../static/swagger-ui.html")) }),
        );
    }

    app = app.fallback({
        let router = frozen_router.clone();
        let state = app_state.clone();
        axum::routing::any(move |req: Request| async move { dispatch(router, state, req).await })
    });

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
        info!("🗜️  Layer: GZip (min: {} bytes)", config.minimum_size);
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
                    crate::http::middleware::execute_py_middleware(middleware, req, next).await
                }
            }));
        }
    }

    // L4: CORS
    if let Some(config) = cors_config {
        info!("🛡️  Layer: CORS");
        match build_cors_layer(&config) {
            Ok(layer) => app = app.layer(layer),
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

async fn dispatch(router: Arc<FrozenRouter>, state: AppState, req: Request) -> Response {
    let method = match req.method().as_str().as_bytes() {
        b"GET" => HttpMethod::GET,
        b"POST" => HttpMethod::POST,
        b"PUT" => HttpMethod::PUT,
        b"DELETE" => HttpMethod::DELETE,
        b"PATCH" => HttpMethod::PATCH,
        b"OPTIONS" => HttpMethod::OPTIONS,
        b"HEAD" => HttpMethod::HEAD,
        _ => return axum::http::StatusCode::METHOD_NOT_ALLOWED.into_response(),
    };

    let path_str = req.uri().path();
    let route_match = match router.resolve(method, path_str) {
        Some(v) => v,
        None => return axum::http::StatusCode::NOT_FOUND.into_response(),
    };

    let (handler, params_iter) = match route_match {
        RouteMatch::Static(handler) => (handler, None),
        RouteMatch::Params(handler, params) => (handler, Some(params)),
    };

    let path_base = path_str.as_ptr() as usize;
    let param_ranges: SmallVec<[PathParamRange; 4]> = if let Some(params) = params_iter {
        params
            .iter()
            .map(|(k, v)| {
                let start = v.as_ptr() as usize - path_base;
                debug_assert!(
                    start <= path_str.len(),
                    "matchit returned a string outside the input path"
                );
                let key_static: &'static str = unsafe { std::mem::transmute(k) };
                PathParamRange {
                    key: key_static,
                    start,
                    end: start + v.len(),
                }
            })
            .collect()
    } else {
        SmallVec::new()
    };

    let (request_parts, body) = req.into_parts();
    let has_body_requirements =
        !handler.body_param_names.is_empty() || !handler.param_validators.is_empty();

    let payload = if has_body_requirements {
        match extract_payload(body).await {
            Ok(p) => p,
            Err(resp) => return resp,
        }
    } else {
        None
    };

    run_py_handler(
        state.rt_handle,
        handler,
        request_parts,
        param_ranges,
        payload,
    )
    .await
}
