use crate::globals::{async_loop_pool, spawn};
use crate::runtime::py_bridge;
use parking_lot::Mutex;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyTuple};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tracing::error;

type TaskFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

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
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[pyo3(signature = (func, *args, **kwargs))]
    fn add_task(
        &self,
        func: Py<PyAny>,
        args: Vec<Py<PyAny>>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        let mut tasks = self.tasks.lock();
        tasks.push((func, args, kwargs.map(|kw| kw.clone().unbind())));
        Ok(())
    }
}

type Task = (Py<PyAny>, Vec<Py<PyAny>>, Option<Py<PyDict>>);

fn spawn_tasks(async_loop: &Arc<Py<PyAny>>, tasks: Vec<Task>) -> Vec<TaskFuture> {
    if tasks.is_empty() {
        return Vec::new();
    }
    let async_loop = async_loop.clone();
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
                        if !result.bind(py).hasattr(intern!(py, "__await__")).unwrap_or(false) {
                            continue;
                        }
                        match py_bridge::schedule_task(py, &async_loop, result.into_bound(py)) {
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
    pub fn execute_all(&self, async_loop: &Arc<Py<PyAny>>) -> Vec<TaskFuture> {
        let tasks = std::mem::take(&mut *self.tasks.lock());
        spawn_tasks(async_loop, tasks)
    }
}

pub fn spawn_response_background(py: Python<'_>, result: &Bound<'_, PyAny>) {
    let Ok(background) = result.getattr(intern!(py, "background")) else {
        return;
    };
    if background.is_none() {
        return;
    }
    let Some(pool) = async_loop_pool() else {
        return;
    };

    let background = background.unbind();
    let async_loop = Python::attach(|py| Arc::new(pool.pick(py)));
    spawn(async move {
        let futures = Python::attach(|py| match background.bind(py).cast::<PyBackgroundTasks>() {
            Ok(tasks) => tasks.borrow().execute_all(&async_loop),
            Err(_) => spawn_tasks(
                &async_loop,
                vec![(background.clone_ref(py), Vec::new(), None)],
            ),
        });
        for task in futures {
            task.await;
        }
    });
}
