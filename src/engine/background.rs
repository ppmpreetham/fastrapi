use crate::runtime::blocking;
use crate::utils::LockExt;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyTuple};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
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
        let mut tasks = self
            .tasks
            .lock()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("Lock error: {}", e)))?;
        tasks.push((func, args, kwargs.map(|kw| kw.clone().unbind())));
        Ok(())
    }
}

impl PyBackgroundTasks {
    pub fn execute_all(&self, async_loop: &Arc<Py<PyAny>>) -> Vec<TaskFuture> {
        let tasks = {
            let mut locked = self.tasks.lock_or_panic();
            std::mem::take(&mut *locked)
        };

        tasks
            .into_iter()
            .map(|(func, args, kwargs)| {
                let async_loop = async_loop.clone();
                Box::pin(async move {
                    let bridged = blocking::run_python(move |py| -> Option<TaskFuture> {
                        let Ok(args_tuple) = PyTuple::new(py, &args) else {
                            error!("Background task: failed to build args tuple");
                            return None;
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
                                if !result.bind(py).hasattr("__await__").unwrap_or(false) {
                                    return None;
                                }
                                let locals = rsloop::rust_async::TaskLocals::new(
                                    async_loop.bind(py).clone(),
                                );
                                match rsloop::rust_async::into_future_with_locals(
                                    &locals,
                                    result.into_bound(py),
                                ) {
                                    Ok(fut) => Some(Box::pin(async move {
                                        if let Err(e) = fut.await {
                                            error!("Async background task error: {}", e);
                                        }
                                    })
                                        as TaskFuture),
                                    Err(e) => {
                                        error!("Background task scheduling error: {}", e);
                                        None
                                    }
                                }
                            }
                            Err(e) => {
                                error!("Background task error: {}", e);
                                e.print(py);
                                None
                            }
                        }
                    })
                    .await;

                    if let Ok(Some(coroutine_future)) = bridged {
                        coroutine_future.await;
                    }
                }) as TaskFuture
            })
            .collect()
    }
}
