//! The start of the HTTP server and the shared application state.
//! This module runs the server, waits for the shutdown signal and supports the reload mode.

use super::lifecycle::{
    log_python_error, run_shutdown_phase, run_startup_phase, start_background_asyncio_loop,
    stop_background_asyncio_loop,
};
use super::routes::build_router;
use fastrapi_core::reload::{ReloadConfig, resolve_reload_dirs, run_reload_supervisor};

use crate::engine::config::FastrAPI;
use axum::Router;
use fastrapi_core::connection::PeerAddrs;
use pyo3::exceptions::PyRuntimeError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyList;
use std::path::PathBuf;
use tracing::{error, info};

use fastrapi_runtime::py_bridge::{self, EventLoop};
use fastrapi_runtime::server_state::set_server_loop;
use fastrapi_runtime::tokio_runtime::PYTHON_RUNTIME;
use std::env::var;
use std::io;
use std::thread::spawn;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigint = signal(SignalKind::interrupt()).expect("Failed to install SIGINT handler");
        let mut sigterm =
            signal(SignalKind::terminate()).expect("Failed to install SIGTERM handler");
        tokio::select! {
            _ = sigint.recv() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(windows)]
    {
        use tokio::signal::windows::{ctrl_break, ctrl_c};
        let mut ctrl_c = ctrl_c().expect("Failed to install Ctrl+C handler");
        let mut ctrl_break = ctrl_break().expect("Failed to install Ctrl+Break handler");
        tokio::select! {
            _ = ctrl_c.recv() => {}
            _ = ctrl_break.recv() => {}
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    }

    info!("Shutting down...");
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "The application state carries several independent server switches"
)]
pub struct AppState {
    pub event_loop: EventLoop,
    pub sync_to_threadpool: bool,
    pub max_body_size: Option<usize>,
    pub max_field_size: Option<usize>,
    pub max_file_size: Option<usize>,
    pub reject_unknown_multipart_fields: bool,
    pub root_path: Box<str>,
    pub metrics_enabled: bool,
    pub redirect_slashes: bool,
    pub app: Py<PyAny>,
}

impl AppState {
    #[must_use]
    pub fn leak(self) -> &'static Self {
        Box::leak(Box::new(self))
    }
}

fn combined_handlers(
    py: Python<'_>,
    app_handlers: Option<&Py<PyAny>>,
    router_handlers: &[Py<PyAny>],
) -> PyResult<Option<Py<PyAny>>> {
    if router_handlers.is_empty() {
        return Ok(app_handlers.map(|handlers| handlers.clone_ref(py)));
    }
    let combined = PyList::empty(py);
    if let Some(handlers) = app_handlers {
        for handler in handlers.bind(py).try_iter()? {
            combined.append(handler?)?;
        }
    }
    for handler in router_handlers {
        combined.append(handler.bind(py))?;
    }
    Ok(Some(combined.into_any().unbind()))
}

pub(crate) struct LifecycleHooks {
    pub(crate) lifespan: Option<Py<PyAny>>,
    pub(crate) on_startup: Option<Py<PyAny>>,
    pub(crate) on_shutdown: Option<Py<PyAny>>,
}

pub(crate) struct PreparedApp {
    pub(crate) router: Router,
    pub(crate) event_loop: EventLoop,
    pub(crate) hooks: LifecycleHooks,
}

pub(crate) fn prepare_app(py: Python<'_>, app: &Py<FastrAPI>) -> PyResult<PreparedApp> {
    py_bridge::init_with_fastrapi_runtime();
    let event_loop = start_background_asyncio_loop(py)?;
    build_app(py, app, event_loop).inspect_err(|_| stop_background_asyncio_loop(py, event_loop))
}

fn build_app(py: Python<'_>, app: &Py<FastrAPI>, event_loop: EventLoop) -> PyResult<PreparedApp> {
    set_server_loop(event_loop);
    event_loop.server().set_app(app.clone_ref(py).into_any());
    let app_bound = app.bind(py);
    app_bound.borrow_mut().apply_router_mounts(py)?;
    let custom_openapi = super::routes::custom_openapi_spec(py, app_bound.as_any());
    let app_config = app_bound.borrow();
    let app_state = AppState {
        event_loop,
        sync_to_threadpool: app_config.sync_to_threadpool,
        max_body_size: app_config.max_body_size,
        max_field_size: app_config.max_field_size,
        max_file_size: app_config.max_file_size,
        reject_unknown_multipart_fields: app_config.reject_unknown_multipart_fields,
        root_path: app_config.root_path.trim_end_matches('/').into(),
        metrics_enabled: app_config.prometheus_config.is_some(),
        redirect_slashes: app_config.redirect_slashes,
        app: app.clone_ref(py).into_any(),
    }
    .leak();
    let hooks = lifecycle_hooks(py, &app_config)?;
    let router = build_router(
        py,
        app_state,
        app_config.docs_url.as_deref(),
        app_config.openapi_url.as_deref(),
        &app_config,
        custom_openapi,
    )?;
    Ok(PreparedApp {
        router,
        event_loop,
        hooks,
    })
}

fn lifecycle_hooks(py: Python<'_>, app_config: &FastrAPI) -> PyResult<LifecycleHooks> {
    let router_events = app_config.router.bind(py).borrow();
    let on_startup = combined_handlers(
        py,
        app_config.on_startup.as_ref(),
        &router_events.startup_handlers.lock(),
    )?;
    let on_shutdown = combined_handlers(
        py,
        app_config.on_shutdown.as_ref(),
        &router_events.shutdown_handlers.lock(),
    )?;
    let router_lifespan = router_events
        .lifespan
        .lock()
        .as_ref()
        .map(|lifespan| lifespan.clone_ref(py));
    drop(router_events);
    let helpers = crate::ffi::compat::py_helpers(py)?;
    let merge = |original: Bound<'_, PyAny>, nested: Py<PyAny>| -> PyResult<Py<PyAny>> {
        helpers
            .getattr(pyo3::intern!(py, "merge_lifespan_context"))?
            .call1((original, nested))
            .map(Bound::unbind)
    };
    let original = app_config
        .lifespan
        .as_ref()
        .map(|handler| handler.clone_ref(py));
    Ok(match (original, router_lifespan) {
        (Some(original), Some(nested)) => LifecycleHooks {
            lifespan: Some(merge(original.into_bound(py), nested)?),
            on_startup,
            on_shutdown,
        },
        (None, Some(nested)) => {
            let handlers = helpers
                .getattr(pyo3::intern!(py, "handlers_lifespan"))?
                .call1((on_startup, on_shutdown))?;
            LifecycleHooks {
                lifespan: Some(merge(handlers, nested)?),
                on_startup: None,
                on_shutdown: None,
            }
        }
        (lifespan, None) => LifecycleHooks {
            lifespan,
            on_startup,
            on_shutdown,
        },
    })
}

pub fn serve(
    py: Python<'_>,
    host: Option<String>,
    port: Option<u16>,
    app: &Py<FastrAPI>,
) -> PyResult<()> {
    let default_filter = "info";
    let filter = var("RUST_LOG").unwrap_or_else(|_| default_filter.to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_new(filter).unwrap_or_default())
        .with_target(false)
        .try_init()
        .ok();

    let host: String = host.unwrap_or_else(|| "127.0.0.1".to_string());
    let port = port.unwrap_or(8000);
    let PreparedApp {
        router,
        event_loop,
        hooks:
            LifecycleHooks {
                lifespan,
                on_startup,
                on_shutdown,
            },
    } = prepare_app(py, app)?;
    let app_config = app.bind(py).borrow();
    let docs_url_for_log = app_config.docs_url.clone();
    let redoc_url = app_config.redoc_url.clone();
    let scalar_url = app_config.scalar_url.clone();
    let elements_url = app_config.elements_url.clone();
    drop(app_config);
    let app = app.clone_ref(py);

    let server_thread = spawn(move || -> PyResult<()> {
        let entered_lifespan =
            Python::attach(|py| run_startup_phase(py, &app, lifespan, on_startup, event_loop));

        let entered_lifespan = match entered_lifespan {
            Ok(entered) => entered,
            Err(err) => {
                log_python_error("startup failed", &err);
                Python::attach(|py| stop_background_asyncio_loop(py, event_loop));
                return Err(err);
            }
        };
        let addr = format!("{host}:{port}");
        let served: io::Result<()> = PYTHON_RUNTIME.block_on(async move {
            let listener = TcpListener::bind(&addr).await.map_err(|err| {
                io::Error::new(err.kind(), format!("failed to bind to {addr}: {err}"))
            })?;

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

            let service = router.into_make_service_with_connect_info::<PeerAddrs>();
            if let Err(err) = axum::serve(listener, service)
                .with_graceful_shutdown(wait_for_shutdown_signal())
                .await
            {
                error!("Server error: {}", err);
            }
            Ok(())
        });

        Python::attach(|py| {
            if let Err(err) = run_shutdown_phase(py, entered_lifespan, on_shutdown, event_loop) {
                log_python_error("shutdown failed", &err);
            }
            stop_background_asyncio_loop(py, event_loop);
        });
        served.map_err(PyErr::from)
    });

    py.detach(move || server_thread.join()).map_err(|panic| {
        let message = panic
            .downcast_ref::<&str>()
            .map(|message| (*message).to_owned())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        PyRuntimeError::new_err(format!("Server thread panicked: {message}"))
    })?
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
    let command: Vec<String> = sys.getattr(intern!(py, "orig_argv"))?.extract()?;
    let command = command.get(1..).unwrap_or_default().to_vec();
    py.detach(move || run_reload_supervisor(&executable, &command, &config))
        .map_err(PyRuntimeError::new_err)
}
