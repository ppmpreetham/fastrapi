use crate::engine::server::*;
use crate::engine::types::{FastrAPI, FrontendMount, StaticMount};
use ahash::{AHashMap, AHashSet};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{ConnectInfo, Request},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header::CONTENT_TYPE},
    middleware::{self as axum_middleware, Next},
    response::{Html, IntoResponse, Response},
    routing::{MethodRouter, *},
    serve::ListenerExt,
};
use dashmap::DashMap;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use notify::{Config as NotifyConfig, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use pyo3::{
    exceptions::{PyRuntimeError, PyTypeError},
    intern,
    prelude::*,
};
use smallvec::SmallVec;
use std::{
    collections::hash_map::Entry,
    net::{IpAddr, SocketAddr},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use tokio::net::TcpListener;
use tower::{ServiceExt, service_fn};
use tower_http::{
    catch_panic::CatchPanicLayer,
    compression::{CompressionLayer, predicate::SizeAbove},
    normalize_path::NormalizePathLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use tower_sessions::{Expiry, MemoryStore, SessionManagerLayer, cookie::Key};
use tracing::{Level, error, info};

use crate::{
    ffi::py_handlers::{
        ExecutionMode, render_no_request_json_response, render_no_request_response, run_py_handler,
        run_py_handler_no_request,
    },
    globals::{MIDDLEWARES, PYTHON_RUNTIME},
    http::{
        middleware::{
            CORSMiddleware, GZipMiddleware, HTTPSRedirectMiddleware, SessionMiddleware,
            TrustedHostMiddleware, build_cors_layer, parse_cors_params, parse_gzip_params,
            parse_https_redirect_params, parse_session_params, parse_trusted_host_params,
        },
        websocket::ws_handler,
    },
    routing::{
        prometheus::prometheus_handle,
        router::{FrozenRouter, FrozenRouterBuilder, RouteMatch},
        types::{BodyField, BodyPayload, HttpMethod, PathParamRange, RouteHandler, UploadedFile},
    },
    utils::{local_guard, openapi::build_openapi_spec, py_any_to_json},
};

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

#[derive(Clone, Hash, PartialEq, Eq)]
pub(crate) struct RateLimitKey {
    handler: usize,
    ip: Option<IpAddr>,
}

pub(crate) struct RateLimitWindow {
    start: Instant,
    count: u32,
}

pub(crate) static RATE_LIMITS: OnceLock<DashMap<RateLimitKey, Mutex<RateLimitWindow>>> = OnceLock::new();

pub(crate) struct EnteredLifespan {
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

pub(crate) fn resolve_reload_dirs(script_path: &str, reload_dirs: Option<Vec<String>>) -> Vec<PathBuf> {
    if let Some(dirs) = reload_dirs {
        return dirs.into_iter().map(PathBuf::from).collect();
    }

    let script = PathBuf::from(script_path);
    if let Some(parent) = script.parent()
        && !parent.as_os_str().is_empty()
    {
        return vec![parent.to_path_buf()];
    }

    std::env::current_dir()
        .map(|dir| vec![dir])
        .unwrap_or_else(|_| vec![PathBuf::from(".")])
}

#[derive(Clone)]
pub(crate) struct ReloadConfig {
    watch_dirs: Vec<PathBuf>,
    ignore_dirs: Vec<PathBuf>,
    ignore_patterns: Vec<String>,
    ignore_paths: Vec<PathBuf>,
    tick_ms: u64,
    ignore_worker_failure: bool,
}

pub(crate) fn run_reload_supervisor(
    executable: &str,
    argv: &[String],
    config: ReloadConfig,
) -> Result<(), String> {
    let ignore_globs = build_reload_ignore_globs(&config)?;
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = RecommendedWatcher::new(
        move |event| {
            let _ = tx.send(event);
        },
        NotifyConfig::default(),
    )
    .map_err(|err| err.to_string())?;

    for dir in &config.watch_dirs {
        watcher
            .watch(dir, RecursiveMode::Recursive)
            .map_err(|err| err.to_string())?;
    }

    let mut child = spawn_reload_child(executable, argv).map_err(|err| err.to_string())?;
    let debounce = Duration::from_millis(config.tick_ms);

    loop {
        if let Some(status) = child.try_wait().map_err(|err| err.to_string())? {
            if config.ignore_worker_failure {
                eprintln!("FastrAPI reload: child exited with status {status}; restarting");
                child = spawn_reload_child(executable, argv).map_err(|err| err.to_string())?;
                continue;
            }
            return Err(format!("reload child exited with status {status}"));
        }

        match rx.recv_timeout(debounce) {
            Ok(Ok(event)) => {
                if !reload_event_matches(&event, &config, &ignore_globs) {
                    continue;
                }
                println!("FastrAPI reload: Python file change detected; restarting server");
                stop_child(&mut child);
                child = spawn_reload_child(executable, argv).map_err(|err| err.to_string())?;
                while rx.try_recv().is_ok() {}
            }
            Ok(Err(err)) => return Err(err.to_string()),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("reload watcher stopped".to_string());
            }
        }
    }
}
pub(crate) fn spawn_reload_child(executable: &str, argv: &[String]) -> std::io::Result<Child> {
    let mut command = Command::new(executable);
    command
        .args(argv)
        .env("FASTRAPI_RELOAD_CHILD", "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command.spawn()
}

pub(crate) fn stop_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub(crate) fn reload_event_matches(
    event: &notify::Event,
    config: &ReloadConfig,
    ignore_globs: &Option<GlobSet>,
) -> bool {
    event.paths.iter().any(|path| {
        path.extension().and_then(|ext| ext.to_str()) == Some("py")
            && !is_reload_ignored(path, config, ignore_globs)
    })
}

pub(crate) fn is_reload_ignored(path: &Path, config: &ReloadConfig, ignore_globs: &Option<GlobSet>) -> bool {
    if path.ancestors().any(is_default_reload_ignored_dir) {
        return true;
    }

    if path.ancestors().any(|ancestor| {
        config.ignore_dirs.iter().any(|ignored| {
            ancestor == ignored
                || ancestor.ends_with(ignored)
                || ignored
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| ancestor.file_name().and_then(|n| n.to_str()) == Some(name))
        })
    }) {
        return true;
    }

    if config
        .ignore_paths
        .iter()
        .any(|ignored| path == ignored || path.ends_with(ignored))
    {
        return true;
    }

    ignore_globs
        .as_ref()
        .is_some_and(|globs| globs.is_match(path))
}

pub(crate) fn is_default_reload_ignored_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, ".git" | ".venv" | "__pycache__" | "target"))
}

pub(crate) fn build_reload_ignore_globs(config: &ReloadConfig) -> Result<Option<GlobSet>, String> {
    if config.ignore_patterns.is_empty() {
        return Ok(None);
    }

    let mut builder = GlobSetBuilder::new();
    for pattern in config
        .ignore_patterns
        .iter()
        .filter(|pattern| !pattern.is_empty())
    {
        let pattern = if pattern.contains(['*', '?', '[', ']']) {
            pattern.clone()
        } else {
            format!("**/*{pattern}*")
        };
        let glob = GlobBuilder::new(&pattern)
            .literal_separator(false)
            .build()
            .map_err(|err| err.to_string())?;
        builder.add(glob);
    }

    builder.build().map(Some).map_err(|err| err.to_string())
}

pub(crate) fn start_background_asyncio_loop(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let loop_module = py.import("rsloop").or_else(|_| py.import("asyncio"))?;
    let event_loop = loop_module.call_method0("new_event_loop")?.unbind();
    let loop_for_thread = event_loop.clone_ref(py);

    std::thread::spawn(move || {
        Python::attach(|py| {
            let _ = py.import("rsloop");
            let Ok(asyncio) = py.import("asyncio") else {
                return;
            };
            let event_loop = loop_for_thread.bind(py);
            let _ = asyncio.call_method1("set_event_loop", (event_loop,));
            if let Err(err) = event_loop.call_method0("run_forever") {
                log_python_error("python async loop stopped with error", err);
            }
            let _ = event_loop.call_method0("close");
        });
    });

    Ok(event_loop)
}

pub(crate) fn stop_background_asyncio_loop(py: Python<'_>, event_loop: &Arc<Py<PyAny>>) {
    let event_loop = event_loop.bind(py);
    if let Ok(stop) = event_loop.getattr("stop") {
        let _ = event_loop.call_method1("call_soon_threadsafe", (stop,));
    }
}

pub(crate) fn run_startup_phase(
    py: Python<'_>,
    app: Py<FastrAPI>,
    lifespan: Option<Py<PyAny>>,
    on_startup: Option<Py<PyAny>>,
) -> PyResult<Option<EnteredLifespan>> {
    if let Some(lifespan_handler) = lifespan {
        return enter_lifespan(py, app, lifespan_handler).map(Some);
    }

    if let Some(startup_handlers) = on_startup {
        run_lifecycle_handlers(py, startup_handlers)?;
    }

    Ok(None)
}

pub(crate) fn run_shutdown_phase(
    py: Python<'_>,
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
pub(crate) fn run_lifecycle_handlers(py: Python<'_>, handlers: Py<PyAny>) -> PyResult<()> {
    extract_lifecycle_handlers(py, &handlers)?
        .into_iter()
        .try_for_each(|handler| run_lifecycle_handler(py, handler))
}

pub(crate) fn extract_lifecycle_handlers<'py>(
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

pub(crate) fn run_lifecycle_handler(py: Python<'_>, handler: Py<PyAny>) -> PyResult<()> {
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

pub(crate) fn enter_lifespan(
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

pub(crate) fn exit_lifespan(py: Python<'_>, entered_lifespan: EnteredLifespan) -> PyResult<()> {
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

pub(crate) fn create_event_loop(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let loop_module = py.import("rsloop").or_else(|_| py.import("asyncio"))?;
    let event_loop = loop_module.call_method0("new_event_loop")?;
    py.import("asyncio")?
        .call_method1("set_event_loop", (&event_loop,))?;
    Ok(event_loop.unbind())
}
pub(crate) fn run_awaitable_in_new_loop(py: Python<'_>, awaitable: Bound<'_, PyAny>) -> PyResult<()> {
    let event_loop = create_event_loop(py)?;
    let result = run_awaitable_in_loop(py, event_loop.bind(py), awaitable);
    shutdown_async_generators(event_loop.bind(py));
    close_event_loop(py, event_loop.bind(py));
    result
}

pub(crate) fn run_awaitable_in_loop(
    py: Python<'_>,
    event_loop: &Bound<'_, PyAny>,
    awaitable: Bound<'_, PyAny>,
) -> PyResult<()> {
    let asyncio = py.import("asyncio")?;
    asyncio.call_method1("set_event_loop", (event_loop,))?;
    event_loop.call_method1("run_until_complete", (awaitable,))?;

    Ok(())
}

pub(crate) fn shutdown_async_generators(event_loop: &Bound<'_, PyAny>) {
    if let Ok(shutdown_asyncgens) = event_loop.call_method0("shutdown_asyncgens") {
        let _ = event_loop.call_method1("run_until_complete", (shutdown_asyncgens,));
    }
}

pub(crate) fn close_event_loop(py: Python<'_>, event_loop: &Bound<'_, PyAny>) {
    if let Ok(asyncio) = py.import("asyncio") {
        let _ = asyncio.call_method1("set_event_loop", (py.None(),));
    }

    let _ = event_loop.call_method0("close");
}

pub(crate) fn log_python_error(context: &str, err: PyErr) {
    error!("{}: {}", context, err);
    Python::attach(|py| err.print(py));
}
