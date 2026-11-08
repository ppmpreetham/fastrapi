//! The bridge between tokio futures and Python asyncio tasks.
//! It keeps the event loop of each request, converts futures between Rust and Python,
//! and schedules Python awaitables on an asyncio event loop.
//! When a `PyTaskFuture` is dropped before it completes, the module cancels the Python task.

use std::future::Future;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3_async_runtimes::tokio as par;

use crate::server_state::ServerContext;
use crate::tokio_runtime::PYTHON_RUNTIME;
pub use pyo3_async_runtimes::{TaskLocals, into_future_with_locals};

#[derive(Clone, Copy)]
pub struct EventLoop(&'static ServerContext);

impl EventLoop {
    pub fn leak(py: Python<'_>, event_loop: Py<PyAny>) -> PyResult<Self> {
        let bridge = pyaio_faq::Bridge::for_loop(py, event_loop.bind(py))?;
        Ok(Self(Box::leak(Box::new(ServerContext::new(
            event_loop, bridge,
        )))))
    }

    #[inline]
    #[must_use]
    pub fn bind(self, py: Python<'_>) -> &Bound<'_, PyAny> {
        self.0.event_loop.bind(py)
    }

    #[inline]
    #[must_use]
    pub const fn as_py(self) -> &'static Py<PyAny> {
        &self.0.event_loop
    }

    #[inline]
    #[must_use]
    pub const fn server(self) -> &'static ServerContext {
        self.0
    }
}

tokio::task_local! {
    static REQUEST_LOOP: EventLoop;
}

pub async fn scoped_request_loop<F: Future>(event_loop: EventLoop, fut: F) -> F::Output {
    REQUEST_LOOP.scope(event_loop, fut).await
}

#[must_use]
pub fn request_loop() -> Option<EventLoop> {
    REQUEST_LOOP.try_with(|event_loop| *event_loop).ok()
}

pub fn init_with_fastrapi_runtime() {
    _ = par::init_with_runtime(&PYTHON_RUNTIME);
    _ = pyaio_faq::tokio::init_with_runtime(&PYTHON_RUNTIME);
}

#[inline]
pub fn get_current_locals(py: Python<'_>) -> PyResult<TaskLocals> {
    par::get_current_locals(py).or_else(|_| {
        request_loop()
            .map(|event_loop| TaskLocals::new(event_loop.bind(py).clone()))
            .ok_or_else(|| PyRuntimeError::new_err("no running event loop"))
    })
}

#[inline]
pub fn future_into_py_with_locals<F, T>(
    py: Python<'_>,
    locals: TaskLocals,
    fut: F,
) -> PyResult<Bound<'_, PyAny>>
where
    F: Future<Output = PyResult<T>> + Send + 'static,
    T: for<'py> IntoPyObject<'py> + Send + 'static,
{
    par::future_into_py_with_locals(py, locals, fut)
}

#[inline]
pub fn future_into_py<F, T>(py: Python<'_>, fut: F) -> PyResult<Bound<'_, PyAny>>
where
    F: Future<Output = PyResult<T>> + Send + 'static,
    T: for<'py> IntoPyObject<'py> + Send + 'static,
{
    par::future_into_py(py, fut)
}

pub type PyTaskFuture = pyaio_faq::CallFuture<Py<PyAny>>;

pub fn schedule_task(
    _py: Python<'_>,
    event_loop: EventLoop,
    awaitable: Bound<'_, PyAny>,
) -> PyResult<PyTaskFuture> {
    let awaitable = awaitable.unbind();
    Ok(event_loop.0.bridge.call(
        move |py| Ok(awaitable.into_bound(py)),
        |obj| Ok(obj.unbind()),
    ))
}
