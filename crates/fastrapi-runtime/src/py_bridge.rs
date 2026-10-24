use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use parking_lot::Mutex;
use pyo3::exceptions::PyRuntimeError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3_async_runtimes::tokio as par;
use tokio::sync::oneshot;

use crate::tokio_runtime::PYTHON_RUNTIME;
pub use pyo3_async_runtimes::{TaskLocals, into_future_with_locals};

fastrapi_pyutil::cached_py_import!(ENSURE_FUTURE, "asyncio", "ensure_future");

#[derive(Clone, Copy)]
pub struct EventLoop(&'static Py<PyAny>);

impl EventLoop {
    #[must_use]
    pub fn leak(event_loop: Py<PyAny>) -> Self {
        Self(Box::leak(Box::new(event_loop)))
    }

    #[inline]
    #[must_use]
    pub fn bind(self, py: Python<'_>) -> &Bound<'_, PyAny> {
        self.0.bind(py)
    }

    #[inline]
    #[must_use]
    pub const fn as_py(self) -> &'static Py<PyAny> {
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

struct TaskHandle {
    event_loop: EventLoop,
    task: Mutex<Option<Py<PyAny>>>,
    cancelled: AtomicBool,
    completed: AtomicBool,
}

#[pyclass]
struct PyTaskBridge {
    awaitable: Option<Py<PyAny>>,
    tx: Option<oneshot::Sender<PyResult<Py<PyAny>>>>,
    handle: Arc<TaskHandle>,
}

#[pymethods]
impl PyTaskBridge {
    #[pyo3(signature = (task=None))]
    fn __call__(slf: &Bound<'_, Self>, task: Option<&Bound<'_, PyAny>>) {
        let py = slf.py();
        match task {
            Some(task) => slf.borrow_mut().complete(py, task),
            None => {
                if let Err(err) = Self::start(slf)
                    && let Some(tx) = slf.borrow_mut().tx.take()
                {
                    _ = tx.send(Err(err));
                }
            }
        }
    }
}

impl PyTaskBridge {
    fn start(slf: &Bound<'_, Self>) -> PyResult<()> {
        let py = slf.py();
        let (awaitable, handle) = {
            let mut this = slf.borrow_mut();
            (this.awaitable.take(), this.handle.clone())
        };
        let Some(awaitable) = awaitable else {
            return Ok(());
        };
        if handle.cancelled.load(Ordering::Relaxed) {
            _ = awaitable.bind(py).call_method0(intern!(py, "close"));
            return Ok(());
        }

        let task = ENSURE_FUTURE.get(py)?.call1((awaitable,))?;
        if handle.cancelled.load(Ordering::Relaxed) {
            _ = task.call_method0(intern!(py, "cancel"));
            return Ok(());
        }
        if let Err(err) = task.call_method1(intern!(py, "add_done_callback"), (slf,)) {
            _ = task.call_method0(intern!(py, "cancel"));
            return Err(err);
        }
        *handle.task.lock() = Some(task.unbind());
        Ok(())
    }

    fn complete(&mut self, py: Python<'_>, task: &Bound<'_, PyAny>) {
        self.handle.completed.store(true, Ordering::Relaxed);
        *self.handle.task.lock() = None;

        let result = task.call_method0(intern!(py, "result")).map(Bound::unbind);
        if let Some(tx) = self.tx.take() {
            _ = tx.send(result);
        }
    }
}

pub struct PyTaskFuture {
    rx: oneshot::Receiver<PyResult<Py<PyAny>>>,
    handle: Arc<TaskHandle>,
}

impl Future for PyTaskFuture {
    type Output = PyResult<Py<PyAny>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.rx).poll(cx).map(|result| {
            result.unwrap_or_else(|_| {
                Err(PyRuntimeError::new_err(
                    "python task was dropped before completing",
                ))
            })
        })
    }
}

impl Drop for PyTaskFuture {
    fn drop(&mut self) {
        if self.handle.completed.load(Ordering::Relaxed) {
            return;
        }
        self.handle.cancelled.store(true, Ordering::Relaxed);

        let Some(task) = self.handle.task.lock().take() else {
            return;
        };

        Python::attach(|py| {
            let Ok(cancel) = task.bind(py).getattr(intern!(py, "cancel")) else {
                return;
            };
            _ = self
                .handle
                .event_loop
                .bind(py)
                .call_method1(intern!(py, "call_soon_threadsafe"), (cancel,));
        });
    }
}

pub fn schedule_task(
    py: Python<'_>,
    event_loop: EventLoop,
    awaitable: Bound<'_, PyAny>,
) -> PyResult<PyTaskFuture> {
    let (tx, rx) = oneshot::channel();
    let handle = Arc::new(TaskHandle {
        event_loop,
        task: Mutex::new(None),
        cancelled: AtomicBool::new(false),
        completed: AtomicBool::new(false),
    });
    let bridge = Bound::new(
        py,
        PyTaskBridge {
            awaitable: Some(awaitable.unbind()),
            tx: Some(tx),
            handle: handle.clone(),
        },
    )?;
    event_loop
        .bind(py)
        .call_method1(intern!(py, "call_soon_threadsafe"), (bridge,))?;
    Ok(PyTaskFuture { rx, handle })
}
