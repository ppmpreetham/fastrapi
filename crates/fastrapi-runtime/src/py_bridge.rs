//! The bridge between tokio futures and Python asyncio tasks.
//! It keeps the event loop of each request, converts futures between Rust and Python,
//! and schedules Python awaitables on an asyncio event loop.
//! When a `PyTaskFuture` is dropped before it completes, the module cancels the Python task.

use std::cell::RefCell;
use std::future::Future;
use std::sync::{Arc, OnceLock};

use pyo3::exceptions::PyRuntimeError;
use pyo3::ffi;
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
            event_loop, bridge, None,
        )))))
    }

    #[must_use]
    pub fn sibling(self, py: Python<'_>) -> Self {
        Self(Box::leak(Box::new(ServerContext::new(
            self.0.event_loop.clone_ref(py),
            Arc::clone(&self.0.bridge),
            Some(self.0),
        ))))
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

/// The `contextvars.Context` a middleware handed to the rest of the stack, as FastAPI copies the
/// middleware's context into the task that runs `call_next`.
#[derive(Clone)]
pub struct RequestContext(Arc<Py<PyAny>>);

impl RequestContext {
    /// Copies the context of the running Python code.
    pub fn capture(py: Python<'_>) -> PyResult<Self> {
        // SAFETY: the thread is attached; a null return sets a Python error.
        let ctx = unsafe { Bound::from_owned_ptr_or_err(py, ffi::PyContext_CopyCurrent())? };
        Ok(Self(Arc::new(ctx.unbind())))
    }
}

/// The request's context, created on first use when no middleware handed one down.
pub type ContextCell = Arc<OnceLock<RequestContext>>;

tokio::task_local! {
    static REQUEST_LOOP: EventLoop;
    static REQUEST_CONTEXT: ContextCell;
}

fn context_cell(context: Option<RequestContext>) -> ContextCell {
    let cell = ContextCell::default();
    if let Some(context) = context {
        _ = cell.set(context);
    }
    cell
}

pub async fn scoped_request_loop<F: Future>(
    event_loop: EventLoop,
    context: Option<RequestContext>,
    fut: F,
) -> F::Output {
    REQUEST_LOOP
        .scope(event_loop, REQUEST_CONTEXT.scope(context_cell(context), fut))
        .await
}

/// Runs `fut` with `context` as the request context; a middleware layer runs in the context the
/// layer above handed down.
pub async fn scoped_request_context<F: Future>(
    context: Option<RequestContext>,
    fut: F,
) -> F::Output {
    REQUEST_CONTEXT.scope(context_cell(context), fut).await
}

#[must_use]
pub fn request_loop() -> Option<EventLoop> {
    REQUEST_LOOP.try_with(|event_loop| *event_loop).ok()
}

thread_local! {
    static JOB_CONTEXT: RefCell<Option<ContextCell>> = const { RefCell::new(None) };
}

/// The request context cell of the running task, or of the blocking job this thread runs.
#[must_use]
pub fn current_context_cell() -> Option<ContextCell> {
    REQUEST_CONTEXT
        .try_with(Arc::clone)
        .ok()
        .or_else(|| JOB_CONTEXT.with_borrow(Clone::clone))
}

/// Runs `job` on this thread as part of the request that owns `cell`.
pub fn with_job_context<R>(cell: Option<ContextCell>, job: impl FnOnce() -> R) -> R {
    let previous = JOB_CONTEXT.replace(cell);
    let result = job();
    JOB_CONTEXT.set(previous);
    result
}

fn new_context(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    // SAFETY: the thread is attached; a null return sets a Python error.
    unsafe { Bound::from_owned_ptr_or_err(py, ffi::PyContext_New()) }
}

fn resolve<'a>(py: Python<'_>, cell: &'a ContextCell) -> PyResult<&'a Py<PyAny>> {
    if let Some(context) = cell.get() {
        return Ok(&context.0);
    }
    let context = RequestContext(Arc::new(new_context(py)?.unbind()));
    Ok(&cell.get_or_init(|| context).0)
}

/// A context entered on this thread until dropped.
pub struct EnteredContext<'py>(Bound<'py, PyAny>);

impl Drop for EnteredContext<'_> {
    fn drop(&mut self) {
        // SAFETY: the context was entered on this thread by `enter_copy` and is still current.
        if unsafe { ffi::PyContext_Exit(self.0.as_ptr()) } != 0 {
            drop(PyErr::fetch(self.0.py()));
        }
    }
}

/// Enters a copy of the request context, as FastAPI runs sync code in a copy of the request's
/// context; outside a request it enters an empty one. Sync user code always runs in one, so
/// nothing it sets reaches later work on the same thread.
pub fn enter_copy<'py>(
    py: Python<'py>,
    cell: Option<&ContextCell>,
) -> PyResult<EnteredContext<'py>> {
    let context = match cell {
        // SAFETY: the thread is attached; a null return sets a Python error.
        Some(cell) => unsafe {
            Bound::from_owned_ptr_or_err(py, ffi::PyContext_Copy(resolve(py, cell)?.as_ptr()))?
        },
        None => new_context(py)?,
    };
    // SAFETY: the thread is attached and `context` is a fresh, unentered context.
    if unsafe { ffi::PyContext_Enter(context.as_ptr()) } != 0 {
        return Err(PyErr::fetch(py));
    }
    Ok(EnteredContext(context))
}

/// `enter_copy` with the request context of the running task.
pub fn enter_request_copy(py: Python<'_>) -> PyResult<EnteredContext<'_>> {
    enter_copy(py, current_context_cell().as_ref())
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
    let py = awaitable.py();
    // Async code runs in the request context itself, as FastAPI awaits it in the request's task.
    let context = match current_context_cell() {
        Some(cell) => Some(resolve(py, &cell)?.clone_ref(py)),
        None => None,
    };
    let awaitable = awaitable.unbind();
    let bridge = &event_loop.0.bridge;
    Ok(match context {
        Some(context) => bridge.call_in_shared(
            context,
            move |py| Ok(awaitable.into_bound(py)),
            |obj| Ok(obj.unbind()),
        ),
        None => bridge.call(move |py| Ok(awaitable.into_bound(py)), |obj| Ok(obj.unbind())),
    })
}
