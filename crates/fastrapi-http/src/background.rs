//! This module contains the `BackgroundTasks` Python class.
//! The class keeps a list of functions and runs them after the server sends the response.

use fastrapi_runtime::py_bridge;
use fastrapi_runtime::py_bridge::EventLoop;
use fastrapi_runtime::server_state::server_loop;
use fastrapi_runtime::tokio_runtime::spawn;
use parking_lot::Mutex;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyTuple};
use std::future::Future;
use std::mem::take;
use std::pin::Pin;
use std::sync::Arc;
use tracing::error;

type TaskFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

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
    #[must_use]
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(Vec::new())),
        }
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
}

type Task = (Py<PyAny>, Vec<Py<PyAny>>, Option<Py<PyDict>>);

fn spawn_tasks(event_loop: EventLoop, tasks: Vec<Task>) -> Vec<TaskFuture> {
    if tasks.is_empty() {
        return Vec::new();
    }
    vec![Box::pin(async move {
        let coroutine_futs: Vec<TaskFuture> = Python::attach(move |py| {
            let mut futs: Vec<TaskFuture> = Vec::new();
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
                            Ok(fut) => futs.push(Box::pin(async move {
                                if let Err(e) = fut.await {
                                    error!("Async background task error: {}", e);
                                }
                            }) as TaskFuture),
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

        for fut in coroutine_futs {
            fut.await;
        }
    }) as TaskFuture]
}

impl PyBackgroundTasks {
    #[must_use]
    pub fn execute_all(&self, event_loop: EventLoop) -> Vec<TaskFuture> {
        let tasks = take(&mut *self.tasks.lock());
        spawn_tasks(event_loop, tasks)
    }
}

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
    let Some(event_loop) = server_loop() else {
        return;
    };
    spawn(async move {
        let futures = Python::attach(|py| {
            background.bind(py).cast::<PyBackgroundTasks>().map_or_else(
                |_| {
                    spawn_tasks(
                        event_loop,
                        vec![(background.clone_ref(py), Vec::new(), None)],
                    )
                },
                |tasks| tasks.borrow().execute_all(event_loop),
            )
        });
        for task in futures {
            task.await;
        }
    });
}
