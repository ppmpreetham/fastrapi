use crate::engine::config::FastrAPI;
use fastrapi_runtime::py_bridge::{EventLoop, schedule_task};
use fastrapi_runtime::server_state::{clear_lifespan_state, set_lifespan_state};
use fastrapi_runtime::tokio_runtime::PYTHON_RUNTIME;
use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use std::thread::Builder;
use tracing::error;

fastrapi_pyutil::cached_py_import!(ASYNCIO_MODULE, "asyncio");

pub(crate) struct EnteredLifespan {
    manager: Py<PyAny>,
    event_loop: EventLoop,
}

pub(crate) fn start_background_asyncio_loop(py: Python<'_>) -> PyResult<EventLoop> {
    let event_loop = create_event_loop(py)?;
    spawn_loop_thread(py, &event_loop)?;
    Ok(EventLoop::leak(event_loop))
}

pub(crate) fn create_event_loop(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let asyncio = ASYNCIO_MODULE.get(py)?;
    let event_loop = asyncio.call_method0("new_event_loop")?;
    asyncio.call_method1("set_event_loop", (&event_loop,))?;
    Ok(event_loop.unbind())
}

fn spawn_loop_thread(py: Python<'_>, event_loop: &Py<PyAny>) -> PyResult<()> {
    let event_loop = event_loop.clone_ref(py);
    Builder::new().name("async-loop".into()).spawn(move || {
        Python::attach(|py| {
            let event_loop = event_loop.bind(py).clone();
            if let Ok(asyncio) = ASYNCIO_MODULE.get(py) {
                _ = asyncio.call_method1("set_event_loop", (&event_loop,));
            }
            if let Err(err) = event_loop.call_method0("run_forever") {
                log_python_error("python async loop stopped with error", &err);
            }
            _ = event_loop.call_method0("close");
        });
    })?;
    Ok(())
}

pub(crate) fn stop_background_asyncio_loop(py: Python<'_>, event_loop: EventLoop) {
    let event_loop = event_loop.bind(py);
    if let Ok(stop) = event_loop.getattr("stop") {
        _ = event_loop.call_method1("call_soon_threadsafe", (stop,));
    }
}

pub(crate) fn run_startup_phase(
    py: Python<'_>,
    app: &Py<FastrAPI>,
    lifespan: Option<Py<PyAny>>,
    on_startup: Option<Py<PyAny>>,
    event_loop: EventLoop,
) -> PyResult<Option<EnteredLifespan>> {
    if let Some(lifespan_handler) = lifespan {
        return enter_lifespan(py, app, &lifespan_handler, event_loop).map(Some);
    }

    if let Some(startup_handlers) = on_startup {
        run_lifecycle_handlers(py, &startup_handlers, event_loop)?;
    }

    Ok(None)
}

pub(crate) fn run_shutdown_phase(
    py: Python<'_>,
    entered_lifespan: Option<EnteredLifespan>,
    on_shutdown: Option<Py<PyAny>>,
    event_loop: EventLoop,
) -> PyResult<()> {
    if let Some(entered) = entered_lifespan {
        return exit_lifespan(py, &entered);
    }

    if let Some(shutdown_handlers) = on_shutdown {
        run_lifecycle_handlers(py, &shutdown_handlers, event_loop)?;
    }

    Ok(())
}
pub(crate) fn run_lifecycle_handlers(
    py: Python<'_>,
    handlers: &Py<PyAny>,
    event_loop: EventLoop,
) -> PyResult<()> {
    extract_lifecycle_handlers(py, handlers)?
        .into_iter()
        .try_for_each(|handler| run_lifecycle_handler(py, &handler, event_loop))
}

pub(crate) fn extract_lifecycle_handlers(
    py: Python<'_>,
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

pub(crate) fn run_lifecycle_handler(
    py: Python<'_>,
    handler: &Py<PyAny>,
    event_loop: EventLoop,
) -> PyResult<()> {
    let handler_bound = handler.bind(py);

    let is_async: bool = py
        .import("inspect")?
        .call_method1("iscoroutinefunction", (handler_bound,))?
        .is_truthy()?;

    if is_async {
        let coroutine = handler_bound.call0()?;
        run_awaitable_in_loop(py, event_loop, coroutine)?;
    } else {
        handler_bound.call0()?;
    }

    Ok(())
}

pub(crate) fn enter_lifespan(
    py: Python<'_>,
    app: &Py<FastrAPI>,
    lifespan: &Py<PyAny>,
    event_loop: EventLoop,
) -> PyResult<EnteredLifespan> {
    let manager = lifespan.bind(py).call1((app.clone_ref(py),))?;
    let awaitable = manager.call_method0("__aenter__")?;

    let state = run_awaitable_in_loop(py, event_loop, awaitable)?;
    if !state.bind(py).is_none() {
        set_lifespan_state(state.clone_ref(py));
    }
    Ok(EnteredLifespan {
        manager: manager.unbind(),
        event_loop,
    })
}

pub(crate) fn exit_lifespan(py: Python<'_>, entered_lifespan: &EnteredLifespan) -> PyResult<()> {
    let awaitable = entered_lifespan
        .manager
        .bind(py)
        .call_method1("__aexit__", (py.None(), py.None(), py.None()))?;

    let result = run_awaitable_in_loop(py, entered_lifespan.event_loop, awaitable);
    clear_lifespan_state();

    result.map(|_| ())
}

pub(crate) fn run_awaitable_in_loop(
    py: Python<'_>,
    event_loop: EventLoop,
    awaitable: Bound<'_, PyAny>,
) -> PyResult<Py<PyAny>> {
    let future = schedule_task(py, event_loop, awaitable)?;
    py.detach(|| PYTHON_RUNTIME.block_on(future))
}

pub(crate) fn log_python_error(context: &str, err: &PyErr) {
    error!("{}: {}", context, err);
    Python::attach(|py| err.print(py));
}
