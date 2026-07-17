use super::lifecycle::*;
use super::reload::*;
use super::routes::*;

use crate::engine::types::FastrAPI;
use axum::serve::ListenerExt;
use pyo3::{exceptions::PyRuntimeError, intern, prelude::*};
use std::{path::PathBuf, sync::Arc};
use tokio::net::TcpListener;
use tracing::{Level, error, info};

use crate::globals::PYTHON_RUNTIME;

#[derive(Clone)]
pub struct AppState {
    pub rt_handle: tokio::runtime::Handle,
    pub async_loop: Arc<Py<PyAny>>,
    pub sync_to_threadpool: bool,
    pub max_body_size: Option<usize>,
    pub max_field_size: Option<usize>,
    pub max_file_size: Option<usize>,
    pub reject_unknown_multipart_fields: bool,
    pub root_path: String,
}
const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn serve(
    py: Python<'_>,
    host: Option<String>,
    port: Option<u16>,
    app: Py<FastrAPI>,
) -> PyResult<()> {
    println!("running on FastRAPI v{}", VERSION);

    tracing_subscriber::fmt()
        .with_max_level(Level::DEBUG)
        .with_target(false)
        .try_init()
        .ok();

    let host: String = host.unwrap_or_else(|| "127.0.0.1".to_string());
    let port = port.unwrap_or(8000);
    let rt_handle = PYTHON_RUNTIME.handle().clone();
    let async_loop = Arc::new(start_background_asyncio_loop(py)?);
    let async_loop_for_shutdown = async_loop.clone();
    let app_bound = app.bind(py);
    let app_config = app_bound.borrow();
    let app_state = AppState {
        rt_handle,
        async_loop,
        sync_to_threadpool: app_config.sync_to_threadpool,
        max_body_size: app_config.max_body_size,
        max_field_size: app_config.max_field_size,
        max_file_size: app_config.max_file_size,
        reject_unknown_multipart_fields: app_config.reject_unknown_multipart_fields,
        root_path: app_config.root_path.clone(),
    };

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
    let redoc_url = app_config.redoc_url.clone();
    let scalar_url = app_config.scalar_url.clone();
    let elements_url = app_config.elements_url.clone();
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
            if let Some(redoc_docs) = &redoc_url {
                info!("📚 ReDoc UI at http://{}{}", addr, redoc_docs);
            }
            if let Some(scalar_docs) = &scalar_url {
                info!("📚 Scalar UI at http://{}{}", addr, scalar_docs);
            }
            if let Some(elements_docs) = &elements_url {
                info!("📚 Elements UI at http://{}{}", addr, elements_docs);
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

        Python::attach(|py| stop_background_asyncio_loop(py, &async_loop_for_shutdown));

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

pub fn serve_with_reload(
    py: Python<'_>,
    reload_dirs: Option<Vec<String>>,
    reload_ignore_dirs: Option<Vec<String>>,
    reload_ignore_patterns: Option<Vec<String>>,
    reload_ignore_paths: Option<Vec<String>>,
    reload_tick: u64,
    reload_ignore_worker_failure: bool,
) -> PyResult<()> {
    println!("running on FastRAPI v{}", VERSION);

    let sys = py.import(intern!(py, "sys"))?;
    let executable: String = sys.getattr(intern!(py, "executable"))?.extract()?;
    let argv: Vec<String> = sys.getattr(intern!(py, "argv"))?.extract()?;
    if argv.is_empty() {
        return Err(PyRuntimeError::new_err(
            "reload=True requires running FastrAPI from a Python script",
        ));
    }

    let watch_dirs = resolve_reload_dirs(&argv[0], reload_dirs);
    let config = ReloadConfig {
        watch_dirs,
        ignore_dirs: reload_ignore_dirs
            .unwrap_or_default()
            .into_iter()
            .map(PathBuf::from)
            .collect(),
        ignore_patterns: reload_ignore_patterns.unwrap_or_default(),
        ignore_paths: reload_ignore_paths
            .unwrap_or_default()
            .into_iter()
            .map(PathBuf::from)
            .collect(),
        tick_ms: reload_tick.max(50),
        ignore_worker_failure: reload_ignore_worker_failure,
    };
    py.detach(move || run_reload_supervisor(&executable, &argv, config))
        .map_err(PyRuntimeError::new_err)
}
