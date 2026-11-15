//! This module contains the `BackgroundTasks` Python class.
//! The class keeps a list of functions and runs them after the server sends the response.

use axum::body::Body;
use axum::response::Response;
use bytes::Bytes;
use fastrapi_runtime::blocking::run_python;
use fastrapi_runtime::py_bridge;
use fastrapi_runtime::py_bridge::EventLoop;
use fastrapi_runtime::server_state::current_server;
use fastrapi_runtime::tokio_runtime::spawn;
use http_body::{Body as HttpBody, Frame, SizeHint};
use parking_lot::Mutex;
use pyo3::exceptions::PyRuntimeError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyTuple};
use std::mem::take;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tracing::error;

fastrapi_pyutil::cached_py_import!(IS_ASYNC_CALLABLE, "starlette._utils", "is_async_callable");

/// A collection of background tasks that will be called after a response has been
/// sent to the client.
///
/// Read more about it in the
/// [FastAPI docs for Background Tasks](https://fastapi.tiangolo.com/tutorial/background-tasks/).
///
/// ## Example
///
/// ```python
/// from fastrapi import BackgroundTasks, FastAPI
///
/// app = FastAPI()
///
///
/// def write_notification(email: str, message=""):
///     with open("log.txt", mode="w") as email_file:
///         content = f"notification for {email}: {message}"
///         email_file.write(content)
///
///
/// @app.post("/send-notification/{email}")
/// async def send_notification(email: str, background_tasks: BackgroundTasks):
///     background_tasks.add_task(write_notification, email, message="some notification")
///     return {"message": "Notification sent in the background"}
/// ```
#[pyclass(name = "BackgroundTasks", skip_from_py_object)]
#[derive(Clone)]
pub struct PyBackgroundTasks {
    tasks: Arc<Mutex<Vec<Task>>>,
}

impl Default for PyBackgroundTasks {
    fn default() -> Self {
        Self::new()
    }
}

#[pymethods]
impl PyBackgroundTasks {
    #[new]
    #[pyo3(signature = (tasks=None))]
    fn py_new(tasks: Option<&Bound<'_, PyAny>>) -> PyResult<Self> {
        let stored = match tasks {
            Some(tasks) => tasks
                .try_iter()?
                .map(|task| Task::from_starlette(&task?))
                .collect::<PyResult<_>>()?,
            None => Vec::new(),
        };
        Ok(Self {
            tasks: Arc::new(Mutex::new(stored)),
        })
    }

    #[getter]
    fn tasks<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let task_class = py
            .import(intern!(py, "starlette.background"))?
            .getattr(intern!(py, "BackgroundTask"))?;
        let tasks = PyList::empty(py);
        for task in &self.snapshot(py) {
            tasks.append(task.to_starlette(py, &task_class)?)?;
        }
        Ok(tasks)
    }

    /// Add a function to be called in the background after the response is sent.
    ///
    /// Read more about it in the
    /// [FastAPI docs for Background Tasks](https://fastapi.tiangolo.com/tutorial/background-tasks/).
    #[pyo3(signature = (func, *args, **kwargs))]
    fn add_task(
        &self,
        func: Bound<'_, PyAny>,
        args: Vec<Py<PyAny>>,
        kwargs: Option<Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        let task = Task::new(func, args, kwargs)?;
        self.tasks.lock().push(task);
        Ok(())
    }

    async fn __call__(&self) -> PyResult<()> {
        let (tasks, locals) = Python::attach(|py| {
            py_bridge::get_current_locals(py).map(|locals| (self.snapshot(py), locals))
        })?;
        for task in tasks {
            task.run(|awaitable| py_bridge::into_future_with_locals(&locals, awaitable))
                .await?;
        }
        Ok(())
    }
}

struct Task {
    func: Py<PyAny>,
    args: Vec<Py<PyAny>>,
    kwargs: Option<Py<PyDict>>,
    is_async: bool,
}

impl Task {
    fn new(
        func: Bound<'_, PyAny>,
        args: Vec<Py<PyAny>>,
        kwargs: Option<Bound<'_, PyDict>>,
    ) -> PyResult<Self> {
        let is_async = IS_ASYNC_CALLABLE
            .get(func.py())?
            .call1((&func,))?
            .is_truthy()?;
        Ok(Self {
            func: func.unbind(),
            args,
            kwargs: kwargs.map(Bound::unbind),
            is_async,
        })
    }

    fn from_starlette(task: &Bound<'_, PyAny>) -> PyResult<Self> {
        let py = task.py();
        Ok(Self {
            func: task.getattr(intern!(py, "func"))?.unbind(),
            args: task.getattr(intern!(py, "args"))?.extract()?,
            kwargs: Some(task.getattr(intern!(py, "kwargs"))?.cast_into::<PyDict>()?.unbind()),
            is_async: task.getattr(intern!(py, "is_async"))?.is_truthy()?,
        })
    }

    fn to_starlette<'py>(
        &self,
        py: Python<'py>,
        task_class: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let args: Vec<&Py<PyAny>> = std::iter::once(&self.func).chain(&self.args).collect();
        task_class.call(PyTuple::new(py, args)?, self.kwargs(py))
    }

    fn clone_ref(&self, py: Python<'_>) -> Self {
        Self {
            func: self.func.clone_ref(py),
            args: self.args.iter().map(|arg| arg.clone_ref(py)).collect(),
            kwargs: self.kwargs.as_ref().map(|kwargs| kwargs.clone_ref(py)),
            is_async: self.is_async,
        }
    }

    fn kwargs<'py>(&self, py: Python<'py>) -> Option<&Bound<'py, PyDict>> {
        self.kwargs.as_ref().map(|kwargs| kwargs.bind(py))
    }

    fn call<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.func
            .bind(py)
            .call(PyTuple::new(py, &self.args)?, self.kwargs(py))
    }

    async fn run<F, Fut>(self, schedule: F) -> PyResult<()>
    where
        F: for<'py> FnOnce(Bound<'py, PyAny>) -> PyResult<Fut>,
        Fut: Future<Output = PyResult<Py<PyAny>>>,
    {
        if self.is_async {
            let pending = Python::attach(|py| schedule(self.call(py)?))?;
            return pending.await.map(drop);
        }
        run_python(move |py| self.call(py).map(drop))
            .await
            .map_err(|panicked| PyRuntimeError::new_err(panicked.to_string()))?
    }
}

/// The tasks of one `BackgroundTasks` object, taken out to run after the response.
pub struct PendingTasks {
    event_loop: EventLoop,
    tasks: Vec<Task>,
}

impl PendingTasks {
    /// Runs the tasks one after another, like Starlette, and stops at the first failure.
    pub async fn run(self) {
        let Self { event_loop, tasks } = self;
        for task in tasks {
            let scheduled = task.run(|awaitable| {
                py_bridge::schedule_task(awaitable.py(), event_loop, awaitable)
            });
            if let Err(err) = scheduled.await {
                Python::attach(|py| err.print(py));
                return;
            }
        }
    }
}

impl PyBackgroundTasks {
    fn snapshot(&self, py: Python<'_>) -> Vec<Task> {
        self.tasks
            .lock()
            .iter()
            .map(|task| task.clone_ref(py))
            .collect()
    }

    #[must_use]
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[must_use]
    pub fn execute_all(&self, event_loop: EventLoop) -> PendingTasks {
        PendingTasks {
            event_loop,
            tasks: take(&mut *self.tasks.lock()),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct OwnBackground;

pub fn spawn_response_background(py: Python<'_>, result: &Bound<'_, PyAny>) {
    let Ok(background) = result.getattr(intern!(py, "background")) else {
        return;
    };
    if background.is_none() {
        return;
    }
    spawn_response_background_value(background.unbind());
}

pub fn spawn_response_background_value(background: Py<PyAny>) {
    if let Some(event_loop) = current_server() {
        spawn(BackgroundJob::Value(background).run(event_loop));
    }
}

pub enum BackgroundJob {
    Tasks(Py<PyBackgroundTasks>),
    Value(Py<PyAny>),
}

impl BackgroundJob {
    pub async fn run(self, event_loop: EventLoop) {
        let pending = Python::attach(|py| match self {
            Self::Tasks(tasks) => match tasks.try_borrow(py) {
                Ok(tasks) => tasks.execute_all(event_loop),
                Err(err) => {
                    error!("Failed to borrow BackgroundTasks: {}", err);
                    PendingTasks {
                        event_loop,
                        tasks: Vec::new(),
                    }
                }
            },
            Self::Value(background) => match background.bind(py).cast::<PyBackgroundTasks>() {
                Ok(tasks) => tasks.borrow().execute_all(event_loop),
                Err(_) => PendingTasks {
                    event_loop,
                    tasks: Task::new(background.into_bound(py), Vec::new(), None)
                        .inspect_err(|err| err.print(py))
                        .into_iter()
                        .collect(),
                },
            },
        });
        pending.run().await;
    }
}

pub trait AfterResponse: Send + 'static {
    fn run(self) -> impl Future<Output = PyResult<()>> + Send;
}

pub struct BackgroundRun(pub BackgroundJob, pub EventLoop);

impl AfterResponse for BackgroundRun {
    async fn run(self) -> PyResult<()> {
        self.0.run(self.1).await;
        Ok(())
    }
}

type JobSlot<J> = Arc<Mutex<Option<J>>>;

pub struct DeferredWork<J>(JobSlot<J>);

impl<J> Clone for DeferredWork<J> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<J> DeferredWork<J> {
    #[must_use]
    pub fn take(&self) -> Option<J> {
        self.0.lock().take()
    }
}

struct RunAfterBody<J: AfterResponse> {
    body: Body,
    job: JobSlot<J>,
}

impl<J: AfterResponse> RunAfterBody<J> {
    fn finish(&self) {
        if let Some(job) = self.job.lock().take() {
            spawn(async move {
                if let Err(err) = job.run().await {
                    Python::attach(|py| err.print(py));
                }
            });
        }
    }
}

impl<J: AfterResponse> Drop for RunAfterBody<J> {
    fn drop(&mut self) {
        self.finish();
    }
}

impl<J: AfterResponse> HttpBody for RunAfterBody<J> {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.body).poll_frame(cx);
        if matches!(polled, Poll::Ready(None)) {
            this.finish();
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

#[must_use]
pub fn run_after_body<J: AfterResponse>(response: Response, job: J) -> Response {
    let slot: JobSlot<J> = Arc::new(Mutex::new(Some(job)));
    let (mut parts, body) = response.into_parts();
    parts.extensions.insert(DeferredWork(Arc::clone(&slot)));
    Response::from_parts(parts, Body::new(RunAfterBody { body, job: slot }))
}
