
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use pyo3::intern;
use pyo3::prelude::*;
use pyo3_async_runtimes::tokio as par;
use tokio::sync::oneshot;

pub use pyo3_async_runtimes::{into_future_with_locals, TaskLocals};

tokio::task_local! {
    static REQUEST_LOOP: Arc<Py<PyAny>>;
}

pub async fn scoped_request_loop<F: Future>(loop_: Arc<Py<PyAny>>, fut: F) -> F::Output {
    REQUEST_LOOP.scope(loop_, fut).await
}

pub fn request_loop() -> Option<Arc<Py<PyAny>>> {
    REQUEST_LOOP.try_with(|l| l.clone()).ok()
}

pub fn init_with_fastrapi_runtime() {
    let _ = par::init_with_runtime(&crate::globals::PYTHON_RUNTIME);
}

#[inline]
pub fn get_current_locals(py: Python<'_>) -> PyResult<TaskLocals> {
    par::get_current_locals(py)
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
    loop_: Py<PyAny>,
    task: parking_lot::Mutex<Option<Py<PyAny>>>,
    cancelled: AtomicBool,
    completed: AtomicBool,
}

#[pyclass]
struct PyTaskSpawner {
    awaitable: Py<PyAny>,
    tx: Option<oneshot::Sender<PyResult<Py<PyAny>>>>,
    handle: Arc<TaskHandle>,
}

#[pymethods]
impl PyTaskSpawner {
    fn __call__(&mut self, py: Python<'_>) {
        if self.handle.cancelled.load(Ordering::Relaxed) {
            _ = self.awaitable.bind(py).call_method0(intern!(py, "close"));
            return;
        }

        if let Err(err) = self.spawn(py)
            && let Some(tx) = self.tx.take()
        {
            _ = tx.send(Err(err));
        }
    }
}

impl PyTaskSpawner {
    fn spawn(&mut self, py: Python<'_>) -> PyResult<()> {
        let asyncio = py.import(intern!(py, "asyncio"))?;
        let task = asyncio.call_method1(
            intern!(py, "ensure_future"),
            (self.awaitable.bind(py),),
        )?;

        if self.handle.cancelled.load(Ordering::Relaxed) {
            _ = task.call_method0(intern!(py, "cancel"));
            return Ok(());
        }

        let completer = match Py::new(
            py,
            PyTaskCompleter {
                tx: self.tx.take(),
                handle: self.handle.clone(),
            },
        ) {
            Ok(completer) => completer,
            Err(err) => {
                _ = task.call_method0(intern!(py, "cancel"));
                return Err(err);
            }
        };

        task.call_method1(intern!(py, "add_done_callback"), (completer,))?;
        *self.handle.task.lock() = Some(task.unbind());
        Ok(())
    }
}

#[pyclass]
struct PyTaskCompleter {
    tx: Option<oneshot::Sender<PyResult<Py<PyAny>>>>,
    handle: Arc<TaskHandle>,
}

#[pymethods]
impl PyTaskCompleter {
    fn __call__(&mut self, py: Python<'_>, task: &Bound<'_, PyAny>) {
        self.handle.completed.store(true, Ordering::Relaxed);
        *self.handle.task.lock() = None;

        let result = task
            .call_method0(intern!(py, "result"))
            .map(Bound::unbind);
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
                Python::attach(|_py| {
                    Err(pyo3::exceptions::PyRuntimeError::new_err(
                        "python task was dropped before completing",
                    ))
                })
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
            return; // spawner hasn't run yet; it will see `cancelled` and close the awaitable
        };

        Python::attach(|py| {
            let Ok(cancel) = task.bind(py).getattr(intern!(py, "cancel")) else {
                return;
            };
            let loop_ = self.handle.loop_.clone_ref(py);
            _ = loop_
                .bind(py)
                .call_method1(intern!(py, "call_soon_threadsafe"), (cancel,));
        });
    }
}

pub fn schedule_task(
    py: Python<'_>,
    loop_: &Py<PyAny>,
    awaitable: Bound<'_, PyAny>,
) -> PyResult<PyTaskFuture> {
    let (tx, rx) = oneshot::channel();
    let handle = Arc::new(TaskHandle {
        loop_: loop_.clone(),
        task: parking_lot::Mutex::new(None),
        cancelled: AtomicBool::new(false),
        completed: AtomicBool::new(false),
    });
    let spawner = Py::new(
        py,
        PyTaskSpawner {
            awaitable: awaitable.unbind(),
            tx: Some(tx),
            handle: handle.clone(),
        },
    )?;
    loop_
        .bind(py)
        .call_method1(intern!(py, "call_soon_threadsafe"), (spawner,))?;
    Ok(PyTaskFuture { rx, handle })
}
