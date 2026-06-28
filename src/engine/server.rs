use super::types::{FastrAPI, StaticMount};
use ahash::{AHashMap, AHashSet};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path as AxumPath, Request},
    http::{HeaderMap, HeaderValue, StatusCode, header::CONTENT_TYPE},
    middleware::{self as axum_middleware, Next},
    response::{Html, IntoResponse, Response},
    routing::{MethodRouter, *},
    serve::ListenerExt,
};
use pyo3::exceptions::{PyRuntimeError, PyTypeError};
use pyo3::intern;
use pyo3::prelude::*;
use smallvec::SmallVec;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::SystemTime;
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
    ffi::py_handlers::{
        ExecutionMode, render_no_request_json_response, render_no_request_response, run_py_handler,
        run_py_handler_no_request,
    },
    routing::{
        router::{FrozenRouter, FrozenRouterBuilder},
        types::RouteHandler,
    },
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
use crate::{
    http::websocket::ws_handler,
    routing::types::{BodyField, BodyPayload, PathParamRange, UploadedFile},
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

fn resolve_reload_dirs(script_path: &str, reload_dirs: Option<Vec<String>>) -> Vec<PathBuf> {
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
struct ReloadConfig {
    watch_dirs: Vec<PathBuf>,
    ignore_dirs: Vec<PathBuf>,
    ignore_patterns: Vec<String>,
    ignore_paths: Vec<PathBuf>,
    tick_ms: u64,
    ignore_worker_failure: bool,
}

fn run_reload_supervisor(
    executable: &str,
    argv: &[String],
    config: ReloadConfig,
) -> Result<(), String> {
    let mut snapshot = snapshot_python_files(&config).map_err(|err| err.to_string())?;
    let mut child = spawn_reload_child(executable, argv).map_err(|err| err.to_string())?;

    loop {
        std::thread::sleep(std::time::Duration::from_millis(config.tick_ms));

        if let Some(status) = child.try_wait().map_err(|err| err.to_string())? {
            if config.ignore_worker_failure {
                eprintln!("FastrAPI reload: child exited with status {status}; restarting");
                child = spawn_reload_child(executable, argv).map_err(|err| err.to_string())?;
                snapshot = snapshot_python_files(&config).map_err(|err| err.to_string())?;
                continue;
            }
            return Err(format!("reload child exited with status {status}"));
        }

        let next_snapshot = snapshot_python_files(&config).map_err(|err| err.to_string())?;
        if next_snapshot == snapshot {
            continue;
        }

        println!("FastrAPI reload: Python file change detected; restarting server");
        stop_child(&mut child);
        child = spawn_reload_child(executable, argv).map_err(|err| err.to_string())?;
        snapshot = next_snapshot;
    }
}

fn spawn_reload_child(executable: &str, argv: &[String]) -> std::io::Result<Child> {
    let mut command = Command::new(executable);
    command
        .args(argv)
        .env("FASTRAPI_RELOAD_CHILD", "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command.spawn()
}

fn stop_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn snapshot_python_files(config: &ReloadConfig) -> std::io::Result<HashMap<PathBuf, FileStamp>> {
    let mut snapshot = HashMap::new();
    for dir in &config.watch_dirs {
        collect_python_files(dir, config, &mut snapshot)?;
    }
    Ok(snapshot)
}

type FileStamp = (Option<SystemTime>, u64);

fn collect_python_files(
    path: &Path,
    config: &ReloadConfig,
    snapshot: &mut HashMap<PathBuf, FileStamp>,
) -> std::io::Result<()> {
    if is_reload_ignored(path, config) {
        return Ok(());
    }

    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(()),
    };

    if metadata.is_file() {
        if path.extension().and_then(|ext| ext.to_str()) == Some("py") {
            snapshot.insert(
                path.to_path_buf(),
                (metadata.modified().ok(), metadata.len()),
            );
        }
        return Ok(());
    }

    if !metadata.is_dir() {
        return Ok(());
    }

    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    if matches!(name, ".git" | ".venv" | "__pycache__" | "target")
        || config.ignore_dirs.iter().any(|ignored| {
            ignored
                .file_name()
                .and_then(|ignored_name| ignored_name.to_str())
                .map(|ignored_name| ignored_name == name)
                .unwrap_or(false)
                || path.ends_with(ignored)
        })
    {
        return Ok(());
    }

    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        collect_python_files(&entry.path(), config, snapshot)?;
    }

    Ok(())
}

fn is_reload_ignored(path: &Path, config: &ReloadConfig) -> bool {
    if config
        .ignore_paths
        .iter()
        .any(|ignored| path == ignored || path.ends_with(ignored))
    {
        return true;
    }

    let path_text = path.to_string_lossy();
    config
        .ignore_patterns
        .iter()
        .any(|pattern| wildcard_match(pattern, &path_text))
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }
    if !pattern.contains('*') {
        return text.contains(pattern);
    }

    let mut remainder = text;
    let mut first = true;
    for part in pattern.split('*').filter(|part| !part.is_empty()) {
        let Some(index) = remainder.find(part) else {
            return false;
        };
        if first && !pattern.starts_with('*') && index != 0 {
            return false;
        }
        remainder = &remainder[index + part.len()..];
        first = false;
    }

    pattern.ends_with('*') || remainder.is_empty()
}

fn start_background_asyncio_loop(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let asyncio = py.import("asyncio")?;
    let event_loop = asyncio.call_method0("new_event_loop")?.unbind();
    let loop_for_thread = event_loop.clone_ref(py);

    std::thread::spawn(move || {
        Python::attach(|py| {
            let Ok(asyncio) = py.import("asyncio") else {
                return;
            };
            let event_loop = loop_for_thread.bind(py);
            let _ = asyncio.call_method1("set_event_loop", (event_loop,));
            if let Err(err) = event_loop.call_method0("run_forever") {
                log_python_error("asyncio loop stopped with error", err);
            }
            let _ = event_loop.call_method0("close");
        });
    });

    Ok(event_loop)
}

fn stop_background_asyncio_loop(py: Python<'_>, event_loop: &Arc<Py<PyAny>>) {
    let event_loop = event_loop.bind(py);
    if let Ok(stop) = event_loop.getattr("stop") {
        let _ = event_loop.call_method1("call_soon_threadsafe", (stop,));
    }
}

fn run_startup_phase(
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

fn run_shutdown_phase(
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

async fn extract_payload(
    headers: &HeaderMap,
    body: Body,
    handler: &RouteHandler,
    state: &AppState,
) -> Result<Option<BodyPayload>, Response> {
    let body = to_bytes(body, state.max_body_size.unwrap_or(usize::MAX))
        .await
        .map_err(|_| (StatusCode::PAYLOAD_TOO_LARGE, "Request body too large").into_response())?;
    if body.is_empty() {
        return Ok(None);
    }

    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");

    if content_type.starts_with("application/x-www-form-urlencoded") {
        return parse_urlencoded_form(&body, state.max_field_size)
            .map(|form| Some(BodyPayload::Form(form)));
    }

    if content_type.starts_with("multipart/form-data") {
        return parse_multipart_form(body, content_type, handler, state)
            .await
            .map(|form| Some(BodyPayload::Form(form)));
    }

    let defer_json_parse = handler.body_param_indices.len() == 1
        && handler.parsed_params[handler.body_param_indices[0]].is_pydantic_model;

    if defer_json_parse {
        return Ok(Some(BodyPayload::Json {
            raw: body,
            value: None,
        }));
    }

    let mut buf = body.to_vec();
    let value = simd_json::serde::from_slice(&mut buf)
        .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "Invalid JSON body").into_response())?;
    Ok(Some(BodyPayload::Json {
        raw: body,
        value: Some(value),
    }))
}

fn parse_urlencoded_form(
    body: &[u8],
    max_field_size: Option<usize>,
) -> Result<ahash::AHashMap<String, BodyField>, Response> {
    let raw = std::str::from_utf8(body)
        .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "Invalid form body").into_response())?;
    let mut form = ahash::AHashMap::new();

    raw.split('&')
        .filter(|pair| !pair.is_empty())
        .try_for_each(|pair| -> Result<(), Response> {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let value = decode_form_component(value);
            if let Some(limit) = max_field_size
                && value.len() > limit
            {
                return Err((StatusCode::PAYLOAD_TOO_LARGE, "Form field too large").into_response());
            }
            form.insert(decode_form_component(key), BodyField::Text(value));
            Ok(())
        })?;

    Ok(form)
}

fn decode_form_component(value: &str) -> String {
    let replaced = value.replace('+', " ");
    percent_encoding::percent_decode_str(&replaced)
        .decode_utf8_lossy()
        .into_owned()
}

async fn parse_multipart_form(
    body: bytes::Bytes,
    content_type: &str,
    handler: &RouteHandler,
    state: &AppState,
) -> Result<ahash::AHashMap<String, BodyField>, Response> {
    let boundary = multer::parse_boundary(content_type)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Missing multipart boundary").into_response())?;
    let stream =
        futures_util::stream::once(
            async move { Ok::<bytes::Bytes, std::convert::Infallible>(body) },
        );
    let constraints = multipart_constraints(handler, state);
    let mut multipart = multer::Multipart::with_constraints(stream, boundary, constraints);
    let mut form = ahash::AHashMap::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(multipart_error_response)?
    {
        let Some(name) = field.name().map(str::to_owned) else {
            continue;
        };
        let filename = field.file_name().map(str::to_owned);
        let content_type = field.content_type().map(ToString::to_string);
        let bytes = field.bytes().await.map_err(multipart_error_response)?;

        if filename.is_some() {
            form.insert(
                name,
                BodyField::File(UploadedFile {
                    filename,
                    content_type,
                    content: bytes.to_vec(),
                }),
            );
        } else {
            form.insert(
                name,
                BodyField::Text(String::from_utf8_lossy(&bytes).into_owned()),
            );
        }
    }

    Ok(form)
}

fn multipart_error_response(err: multer::Error) -> Response {
    match err {
        multer::Error::FieldSizeExceeded { .. } | multer::Error::StreamSizeExceeded { .. } => {
            (StatusCode::PAYLOAD_TOO_LARGE, err.to_string()).into_response()
        }
        multer::Error::UnknownField { .. } => {
            (StatusCode::BAD_REQUEST, err.to_string()).into_response()
        }
        _ => (StatusCode::BAD_REQUEST, "Invalid multipart body").into_response(),
    }
}

fn multipart_constraints(handler: &RouteHandler, state: &AppState) -> multer::Constraints {
    let mut size_limit = multer::SizeLimit::new();

    if let Some(limit) = state.max_body_size {
        size_limit = size_limit.whole_stream(limit as u64);
    }

    match (state.max_field_size, state.max_file_size) {
        (Some(field), Some(file)) => {
            size_limit = size_limit.per_field(field.max(file) as u64);
        }
        (Some(field), None) => {
            size_limit = size_limit.per_field(field as u64);
        }
        (None, Some(file)) => {
            size_limit = size_limit.per_field(file as u64);
        }
        (None, None) => {}
    }

    let mut allowed = Vec::new();
    for param in handler
        .parsed_params
        .iter()
        .filter(|p| matches!(p.source, crate::routing::types::ParameterSource::Body))
    {
        allowed.push(param.external_name.clone());
        if param.external_name != param.name {
            allowed.push(param.name.clone());
        }

        let limit = if is_file_param(param) {
            state.max_file_size
        } else {
            state.max_field_size
        };
        if let Some(limit) = limit {
            size_limit = size_limit.for_field(param.external_name.clone(), limit as u64);
            if param.external_name != param.name {
                size_limit = size_limit.for_field(param.name.clone(), limit as u64);
            }
        }
    }

    let mut constraints = multer::Constraints::new().size_limit(size_limit);
    if state.reject_unknown_multipart_fields && !allowed.is_empty() {
        constraints = constraints.allowed_fields(allowed);
    }
    constraints
}

fn is_file_param(param: &crate::routing::types::ParsedParameter) -> bool {
    let default_is_file = param
        .param_object
        .as_ref()
        .and_then(|obj| {
            Python::attach(|py| {
                obj.bind(py)
                    .get_type()
                    .name()
                    .ok()
                    .map(|name| name.to_string_lossy().into_owned())
            })
        })
        .map(|name| name == "File")
        .unwrap_or(false);

    let annotation_is_upload = param
        .annotation
        .as_ref()
        .and_then(|annotation| {
            Python::attach(|py| {
                annotation
                    .bind(py)
                    .getattr(intern!(py, "__name__"))
                    .ok()
                    .and_then(|name| name.extract::<String>().ok())
            })
        })
        .map(|name| name.contains("UploadFile"))
        .unwrap_or(false);

    default_is_file || annotation_is_upload
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

#[derive(Clone)]
struct CachedResponse {
    status: StatusCode,
    headers: CachedHeaders,
    body: bytes::Bytes,
}

#[derive(Clone)]
enum CachedHeaders {
    Empty,
    ContentType(HeaderValue),
    Full(HeaderMap),
}

impl CachedResponse {
    fn to_response(&self) -> Response {
        let mut response = Body::from(self.body.clone()).into_response();
        *response.status_mut() = self.status;
        match &self.headers {
            CachedHeaders::Empty => {}
            CachedHeaders::ContentType(content_type) => {
                response
                    .headers_mut()
                    .insert(CONTENT_TYPE, content_type.clone());
            }
            CachedHeaders::Full(headers) => {
                *response.headers_mut() = headers.clone();
            }
        }
        response
    }
}

fn cached_headers(headers: &HeaderMap) -> CachedHeaders {
    if headers.is_empty() {
        return CachedHeaders::Empty;
    }

    if headers.len() == 1
        && let Some(content_type) = headers.get(CONTENT_TYPE)
    {
        return CachedHeaders::ContentType(content_type.clone());
    }

    CachedHeaders::Full(headers.clone())
}

fn precompute_const_response(
    py: Python<'_>,
    handler: &Arc<RouteHandler>,
) -> Option<Arc<CachedResponse>> {
    let response = render_no_request_response(py, handler);
    let status = response.status();
    let headers = cached_headers(response.headers());
    let body = PYTHON_RUNTIME
        .block_on(to_bytes(response.into_body(), usize::MAX))
        .ok()?;

    Some(Arc::new(CachedResponse {
        status,
        headers,
        body,
    }))
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

    let mut cached_routes: AHashMap<String, MethodRouter> = AHashMap::new();
    flat.0
        .iter()
        .filter(|route| {
            route.handler.cache_response
                && !route.path.contains('{')
                && matches!(route.handler.execution_mode, ExecutionMode::SyncNoArgs)
        })
        .filter_map(|route| {
            precompute_const_response(py, &route.handler)
                .map(|cached| (route.path.clone(), route.method, cached))
        })
        .for_each(|(path, method, cached)| {
            let method_router = cached_method_router(method, cached);
            match cached_routes.entry(path) {
                Entry::Occupied(mut entry) => {
                    let merged = entry.get().clone().merge(method_router);
                    *entry.get_mut() = merged;
                }
                Entry::Vacant(entry) => {
                    entry.insert(method_router);
                }
            }
        });

    app = cached_routes
        .into_iter()
        .fold(app, |current_app, (path, method_router)| {
            current_app.route(&path, method_router)
        });

    let mut direct_no_request_routes: AHashMap<String, MethodRouter> = AHashMap::new();
    flat.0
        .iter()
        .filter(|route| {
            !route.handler.cache_response
                && !route.path.contains('{')
                && matches!(
                    route.handler.execution_mode,
                    ExecutionMode::SyncNoArgs | ExecutionMode::AsyncNoArgs
                )
        })
        .for_each(|route| {
            let method_router =
                no_request_method_router(route.method, route.handler.clone(), app_state.clone());
            match direct_no_request_routes.entry(route.path.clone()) {
                Entry::Occupied(mut entry) => {
                    let merged = entry.get().clone().merge(method_router);
                    *entry.get_mut() = merged;
                }
                Entry::Vacant(entry) => {
                    entry.insert(method_router);
                }
            }
        });

    app = direct_no_request_routes
        .into_iter()
        .fold(app, |current_app, (path, method_router)| {
            current_app.route(&path, method_router)
        });

    app = flat.1.iter().fold(app, |current_app, ws| {
        let path = ws.path.clone();
        let handler = Arc::new(ws.handler.clone_ref(py));
        let rt_handle = app_state.rt_handle.clone();
        let async_loop = app_state.async_loop.clone();

        current_app.route(
            &path,
            axum::routing::get(move |ws_upgrade| {
                ws_handler(
                    ws_upgrade,
                    axum::Extension(handler.clone()),
                    axum::Extension(rt_handle.clone()),
                    axum::Extension(async_loop.clone()),
                )
            }),
        )
    });

    app = app_config
        .static_mounts
        .iter()
        .cloned()
        .fold(app, |current_app, mount| {
            add_static_mount(current_app, mount)
        });

    let openapi_json = Arc::new(build_openapi_spec(py, app_config));

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
        let middlewares = Arc::new(
            MIDDLEWARES
                .iter(&guard)
                .map(|(_key, middleware_ref)| middleware_ref.clone())
                .collect::<Vec<_>>(),
        );
        app =
            app.layer(axum_middleware::from_fn(move |req, next| {
                let middlewares = middlewares.clone();
                async move {
                    crate::http::middleware::execute_py_middlewares(middlewares, req, next).await
                }
            }));
    }

    // L4: Trusted Host
    if let Some(config) = Option::<CORSMiddleware>::None {
        info!("🛡️  Layer: CORS");
        match build_cors_layer(&config) {
            Ok(layer) => app = app.layer(layer),
            Err(e) => eprintln!("Error building CORS layer: {:?}", e),
        }
    }

    // L5: Trusted Host
    if let Some(config) = trusted_host_config {
        info!("🔒 Layer: TrustedHost");
        let allow_all = config.allowed_hosts.iter().any(|host| host == "*");

        if !allow_all {
            let allowed: Arc<AHashSet<String>> =
                Arc::new(config.allowed_hosts.into_iter().collect());
            let redirect = config.www_redirect;

            app = app.layer(axum_middleware::from_fn(move |req: Request, next: Next| {
                let allowed = allowed.clone();
                async move {
                    let host_header = req
                        .headers()
                        .get("host")
                        .and_then(|h| h.to_str().ok())
                        .unwrap_or("");

                    if allowed.contains(host_header) {
                        return next.run(req).await;
                    }

                    if redirect && host_header.starts_with("www.") {
                        let root = host_header.strip_prefix("www.").unwrap();
                        if allowed.contains(root) {
                            return (axum::http::StatusCode::MOVED_PERMANENTLY, "Redirecting...")
                                .into_response();
                        }
                    }

                    (StatusCode::BAD_REQUEST, "Invalid Host Header").into_response()
                }
            }));
        }
    }

    // L5: CORS. Apply last so preflight can terminate before other layers.
    if let Some(config) = cors_config {
        info!("Layer: CORS");
        match build_cors_layer(&config) {
            Ok(layer) => app = app.layer(layer),
            Err(e) => eprintln!("Error building CORS layer: {:?}", e),
        }
    }

    app
}

fn cached_method_router(method: HttpMethod, cached: Arc<CachedResponse>) -> MethodRouter {
    macro_rules! route_for {
        ($method_fn:ident) => {{
            let cached = cached.clone();
            $method_fn(move || {
                let cached = cached.clone();
                async move { cached.to_response() }
            })
        }};
    }

    match method {
        HttpMethod::GET => route_for!(get),
        HttpMethod::POST => route_for!(post),
        HttpMethod::PUT => route_for!(put),
        HttpMethod::DELETE => route_for!(delete),
        HttpMethod::PATCH => route_for!(patch),
        HttpMethod::OPTIONS => route_for!(options),
        HttpMethod::HEAD => route_for!(head),
    }
}

fn add_static_mount(app: Router, mount: StaticMount) -> Router {
    let mount_path = if mount.path == "/" {
        String::new()
    } else {
        mount.path.trim_end_matches('/').to_string()
    };
    let root_route = if mount_path.is_empty() {
        "/".to_string()
    } else {
        mount_path.clone()
    };
    let wildcard_route = if mount_path.is_empty() {
        "/{*path}".to_string()
    } else {
        format!("{mount_path}/{{*path}}")
    };

    let mount = Arc::new(mount);
    let root_mount = mount.clone();
    let wildcard_mount = mount;

    app.route(
        &root_route,
        get(move || {
            let mount = root_mount.clone();
            async move { serve_static_file(mount, String::new()).await }
        }),
    )
    .route(
        &wildcard_route,
        get(move |AxumPath(path): AxumPath<String>| {
            let mount = wildcard_mount.clone();
            async move { serve_static_file(mount, path).await }
        }),
    )
}

async fn serve_static_file(mount: Arc<StaticMount>, request_path: String) -> Response {
    let Some(mut file_path) = resolve_static_path(&mount, &request_path) else {
        return StatusCode::FORBIDDEN.into_response();
    };

    let mut metadata = match tokio::fs::metadata(&file_path).await {
        Ok(metadata) => metadata,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    if metadata.is_dir() {
        if !mount.html {
            return StatusCode::NOT_FOUND.into_response();
        }
        file_path.push("index.html");
        metadata = match tokio::fs::metadata(&file_path).await {
            Ok(metadata) => metadata,
            Err(_) => return StatusCode::NOT_FOUND.into_response(),
        };
    }

    if !metadata.is_file() {
        return StatusCode::NOT_FOUND.into_response();
    }

    if !mount.follow_symlink {
        if let Ok(link_metadata) = tokio::fs::symlink_metadata(&file_path).await
            && link_metadata.file_type().is_symlink()
        {
            return StatusCode::FORBIDDEN.into_response();
        }
    }

    let bytes = match tokio::fs::read(&file_path).await {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let content_type = static_content_type(&file_path);
    let mut response = Body::from(bytes).into_response();
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

fn resolve_static_path(mount: &StaticMount, request_path: &str) -> Option<PathBuf> {
    let decoded = percent_encoding::percent_decode_str(request_path)
        .decode_utf8()
        .ok()?;
    let mut file_path = PathBuf::from(&mount.directory);

    for component in Path::new(decoded.as_ref()).components() {
        match component {
            Component::Normal(part) => file_path.push(part),
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => return None,
        }
    }

    Some(file_path)
}

fn static_content_type(path: &Path) -> &'static str {
    let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");

    if ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm") {
        "text/html; charset=utf-8"
    } else if ext.eq_ignore_ascii_case("css") {
        "text/css; charset=utf-8"
    } else if ext.eq_ignore_ascii_case("js") || ext.eq_ignore_ascii_case("mjs") {
        "text/javascript; charset=utf-8"
    } else if ext.eq_ignore_ascii_case("json") {
        "application/json"
    } else if ext.eq_ignore_ascii_case("png") {
        "image/png"
    } else if ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg") {
        "image/jpeg"
    } else if ext.eq_ignore_ascii_case("gif") {
        "image/gif"
    } else if ext.eq_ignore_ascii_case("svg") {
        "image/svg+xml"
    } else if ext.eq_ignore_ascii_case("webp") {
        "image/webp"
    } else if ext.eq_ignore_ascii_case("ico") {
        "image/x-icon"
    } else if ext.eq_ignore_ascii_case("txt") {
        "text/plain; charset=utf-8"
    } else if ext.eq_ignore_ascii_case("wasm") {
        "application/wasm"
    } else if ext.eq_ignore_ascii_case("pdf") {
        "application/pdf"
    } else {
        "application/octet-stream"
    }
}

fn no_request_method_router(
    method: HttpMethod,
    handler: Arc<crate::routing::types::RouteHandler>,
    state: AppState,
) -> MethodRouter {
    if matches!(handler.execution_mode, ExecutionMode::SyncNoArgs) && !state.sync_to_threadpool {
        return sync_no_request_method_router(method, handler);
    }

    macro_rules! route_for {
        ($method_fn:ident) => {{
            let handler = handler.clone();
            let state = state.clone();
            $method_fn(move || {
                let handler = handler.clone();
                let state = state.clone();
                async move {
                    run_py_handler_no_request(
                        state.rt_handle,
                        state.async_loop,
                        state.sync_to_threadpool,
                        handler,
                    )
                    .await
                }
            })
        }};
    }

    match method {
        HttpMethod::GET => route_for!(get),
        HttpMethod::POST => route_for!(post),
        HttpMethod::PUT => route_for!(put),
        HttpMethod::DELETE => route_for!(delete),
        HttpMethod::PATCH => route_for!(patch),
        HttpMethod::OPTIONS => route_for!(options),
        HttpMethod::HEAD => route_for!(head),
    }
}

fn sync_no_request_method_router(
    method: HttpMethod,
    handler: Arc<crate::routing::types::RouteHandler>,
) -> MethodRouter {
    let use_json_fast_path = handler.response_model.is_none()
        && handler.response_class.is_none()
        && matches!(
            handler.response_type,
            crate::types::response::ResponseType::Json
        );

    macro_rules! route_for {
        ($method_fn:ident) => {{
            let handler = handler.clone();
            $method_fn(move || {
                let handler = handler.clone();
                async move {
                    Python::attach(|py| {
                        if use_json_fast_path {
                            render_no_request_json_response(py, &handler)
                        } else {
                            render_no_request_response(py, &handler)
                        }
                    })
                }
            })
        }};
    }

    match method {
        HttpMethod::GET => route_for!(get),
        HttpMethod::POST => route_for!(post),
        HttpMethod::PUT => route_for!(put),
        HttpMethod::DELETE => route_for!(delete),
        HttpMethod::PATCH => route_for!(patch),
        HttpMethod::OPTIONS => route_for!(options),
        HttpMethod::HEAD => route_for!(head),
    }
}

async fn dispatch(router: Arc<FrozenRouter>, state: AppState, req: Request) -> Response {
    let method = match req.method().as_str() {
        "GET" => HttpMethod::GET,
        "POST" => HttpMethod::POST,
        "PUT" => HttpMethod::PUT,
        "DELETE" => HttpMethod::DELETE,
        "PATCH" => HttpMethod::PATCH,
        "OPTIONS" => HttpMethod::OPTIONS,
        "HEAD" => HttpMethod::HEAD,
        _ => return axum::http::StatusCode::METHOD_NOT_ALLOWED.into_response(),
    };

    let original_path = req.uri().path();
    let root = state.root_path.trim_end_matches('/');
    let path_str = if root.is_empty() {
        original_path
    } else if original_path == root {
        "/"
    } else if let Some(stripped) = original_path.strip_prefix(root) {
        if stripped.starts_with('/') {
            stripped
        } else {
            return StatusCode::NOT_FOUND.into_response();
        }
    } else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let route_match = match router.resolve(method, path_str) {
        Some(v) => v,
        None => return axum::http::StatusCode::NOT_FOUND.into_response(),
    };

    let (handler, params_iter) = match route_match {
        RouteMatch::Static(handler) => (handler, None),
        RouteMatch::Params(handler, params) => (handler, Some(params)),
    };

    if matches!(
        handler.execution_mode,
        ExecutionMode::SyncNoArgs | ExecutionMode::AsyncNoArgs
    ) {
        return run_py_handler_no_request(
            state.rt_handle,
            state.async_loop,
            state.sync_to_threadpool,
            handler,
        )
        .await;
    }

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
                PathParamRange {
                    key: k.to_string(),
                    start,
                    end: start + v.len(),
                }
            })
            .collect()
    } else {
        SmallVec::new()
    };

    let (request_parts, body) = req.into_parts();
    let has_body_requirements = !handler.body_param_indices.is_empty();

    let payload = if has_body_requirements {
        match extract_payload(&request_parts.headers, body, &handler, &state).await {
            Ok(p) => p,
            Err(resp) => return resp,
        }
    } else {
        None
    };

    run_py_handler(
        state.rt_handle,
        state.async_loop,
        state.sync_to_threadpool,
        handler,
        request_parts,
        param_ranges,
        payload,
    )
    .await
}
