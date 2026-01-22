use pyo3::prelude::*;
use pyo3::types::{PyAny, PyTuple};
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;
use tracing::error;

#[pyclass(name = "BackgroundTasks")]
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

        let mut handles = Vec::new();

        for (func, args) in tasks {
            let handle = tokio::spawn(async move {
                Python::attach(|py| {
                    let args_tuple = PyTuple::new(py, &args).expect("Failed to create tuple");
                    match func.call1(py, &args_tuple) {
                        Ok(_) => {}
                        Err(e) => {
                            error!("Background task error: {}", e);
                            e.print(py);
                        }
                    }
                });
            });
            handles.push(handle);
        }

        handles
    }
}

pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    let bg_module = PyModule::new(py, "background")?;
    bg_module.add_class::<PyBackgroundTasks>()?;

    parent.add_submodule(&bg_module)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("fastrapi.background", &bg_module)?;

    Ok(())
}
