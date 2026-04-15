use pyo3::prelude::*;
use pyo3::types::{PyAny, PyTuple};
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;
use tracing::error;

#[pyclass(name = "BackgroundTasks", skip_from_py_object)]
#[derive(Clone)]
pub struct PyBackgroundTasks {
    tasks: Arc<Mutex<Vec<(Py<PyAny>, Vec<Py<PyAny>>)>>>,
}

#[pymethods]
impl PyBackgroundTasks {
    #[new]
    fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn add_task(&self, func: Py<PyAny>, args: Vec<Py<PyAny>>) -> PyResult<()> {
        let mut tasks = self
            .tasks
            .lock()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("Lock error: {}", e)))?;
        tasks.push((func, args));
        Ok(())
    }
}

impl PyBackgroundTasks {
    // this is called AFTER response is sent
    pub async fn execute_all(&self) -> Vec<JoinHandle<()>> {
        let tasks = {
            let locked = self.tasks.lock().expect("Background tasks lock poisoned");
            locked.clone()
        };

        let mut handles = Vec::with_capacity(tasks.len());

        for (func, args) in tasks {
            let handle = tokio::task::spawn_blocking(move || {
                Python::attach(|py| {
                    let args_tuple = match PyTuple::new(py, &args) {
                        Ok(t) => t,
                        Err(e) => {
                            error!("Background task: failed to build args tuple: {}", e);
                            return;
                        }
                    };
                    if let Err(e) = func.into_bound(py).call1(&args_tuple) {
                        error!("Background task error: {}", e);
                        e.print(py);
                    }
                    drop(args_tuple);
                });
            });
            handles.push(handle);
        }

        handles
    }
}
