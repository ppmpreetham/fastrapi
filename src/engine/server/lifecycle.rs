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

pub(crate) struct EnteredLifespan {
    manager: Py<PyAny>,
    event_loop: Py<PyAny>,
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
