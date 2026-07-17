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
