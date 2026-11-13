//! This module contains the `BackgroundTasks` Python class.
//! The class keeps a list of functions and runs them after the server sends the response.

use fastrapi_runtime::py_bridge;
use fastrapi_runtime::py_bridge::EventLoop;
use fastrapi_runtime::server_state::current_server;
use fastrapi_runtime::tokio_runtime::spawn;
use parking_lot::Mutex;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyTuple};
use std::mem::take;
use std::sync::Arc;
use tracing::error;

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
    tasks: Arc<Mutex<Vec<(Py<PyAny>, Vec<Py<PyAny>>, Option<Py<PyDict>>)>>>,
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
        let mut stored = Vec::new();
        if let Some(tasks) = tasks {
            for task in tasks.try_iter()? {
                let task = task?;
                let py = task.py();
                let kwargs = task.getattr(intern!(py, "kwargs"))?;
                let kwargs = if kwargs.is_none() {
                    None
                } else {
                    Some(kwargs.cast_into::<PyDict>()?.unbind())
                };
                stored.push((
                    task.getattr(intern!(py, "func"))?.unbind(),
                    task.getattr(intern!(py, "args"))?.extract()?,
                    kwargs,
                ));
            }
        }
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
        for (func, args, kwargs) in &self.snapshot(py) {
            let mut call_args = vec![func.clone_ref(py)];
            call_args.extend(args.iter().map(|arg| arg.clone_ref(py)));
            tasks.append(task_class.call(
                PyTuple::new(py, call_args)?,
                kwargs.as_ref().map(|kwargs| kwargs.bind(py)),
            )?)?;
        }
        Ok(tasks)
    }

    /// Add a function to be called in the background after the response is sent.
    ///
    /// Read more about it in the
    /// [FastAPI docs for Background Tasks](https://fastapi.tiangolo.com/tutorial/background-tasks/).
    #[pyo3(signature = (func, *args, **kwargs))]
    fn add_task(&self, func: Py<PyAny>, args: Vec<Py<PyAny>>, kwargs: Option<&Bound<'_, PyDict>>) {
        let mut tasks = self.tasks.lock();
        tasks.push((func, args, kwargs.map(|kw| kw.clone().unbind())));
    }

    async fn __call__(&self) -> PyResult<()> {
        let (tasks, locals) = Python::attach(|py| {
            py_bridge::get_current_locals(py).map(|locals| (self.snapshot(py), locals))
        })?;
        for (func, args, kwargs) in tasks {
            let pending = Python::attach(|py| -> PyResult<_> {
                let kwargs = kwargs.as_ref().map(|kwargs| kwargs.bind(py));
                let result = func.bind(py).call(PyTuple::new(py, args)?, kwargs)?;
                if result.hasattr(intern!(py, "__await__"))? {
                    return py_bridge::into_future_with_locals(&locals, result).map(Some);
                }
                Ok(None)
            })?;
            if let Some(pending) = pending {
                pending.await?;
            }
        }
        Ok(())
    }
}

type Task = (Py<PyAny>, Vec<Py<PyAny>>, Option<Py<PyDict>>);

/// The tasks of one `BackgroundTasks` object, taken out to run after the response.
pub struct PendingTasks {
    event_loop: EventLoop,
    tasks: Vec<Task>,
}

impl PendingTasks {
    /// Runs the synchronous tasks, then waits for the asynchronous tasks in order.
    pub async fn run(self) {
        if self.tasks.is_empty() {
            return;
        }
        let Self { event_loop, tasks } = self;
        let futures: Vec<py_bridge::PyTaskFuture> = Python::attach(move |py| {
            let mut futs = Vec::new();
            for (func, args, kwargs) in tasks {
                let Ok(args_tuple) = PyTuple::new(py, &args) else {
                    error!("Background task: failed to build args tuple");
                    continue;
                };
                let call_result = match kwargs {
                    Some(kw) => func
                        .into_bound(py)
                        .call(args_tuple, Some(kw.bind(py)))
                        .map(Bound::unbind),
                    None => func.into_bound(py).call1(&args_tuple).map(Bound::unbind),
                };
                match call_result {
                    Ok(result) => {
                        if !result
                            .bind(py)
                            .hasattr(intern!(py, "__await__"))
                            .unwrap_or(false)
                        {
                            continue;
                        }
                        match py_bridge::schedule_task(py, event_loop, result.into_bound(py)) {
                            Ok(fut) => futs.push(fut),
                            Err(e) => {
                                error!("Background task scheduling error: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        error!("Background task error: {}", e);
                        e.print(py);
                    }
                }
            }
            futs
        });
        for future in futures {
            if let Err(e) = future.await {
                error!("Async background task error: {}", e);
            }
        }
    }
}

impl PyBackgroundTasks {
    fn snapshot(&self, py: Python<'_>) -> Vec<Task> {
        self.tasks
            .lock()
            .iter()
            .map(|(func, args, kwargs)| {
                (
                    func.clone_ref(py),
                    args.iter().map(|arg| arg.clone_ref(py)).collect(),
                    kwargs.as_ref().map(|kwargs| kwargs.clone_ref(py)),
                )
            })
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
    let Some(event_loop) = current_server() else {
        return;
    };
    spawn(async move {
        let pending = Python::attach(|py| {
            background.bind(py).cast::<PyBackgroundTasks>().map_or_else(
                |_| PendingTasks {
                    event_loop,
                    tasks: vec![(background.clone_ref(py), Vec::new(), None)],
                },
                |tasks| tasks.borrow().execute_all(event_loop),
            )
        });
        pending.run().await;
    });
}
