use pyo3::prelude::*;
use once_cell::sync::Lazy;
use pyo3::types::{PyCFunction, PyDict, PyTuple};
use std::collections::HashMap;
use std::sync::Mutex;
// use hyper::service::service_fn;

// lazy init of mutex of hashmap : /about -> function
static ROUTES: Lazy<Mutex<HashMap<String, Py<PyAny>>>> = Lazy::new(|| Mutex::new(HashMap::new()));

#[pyclass]
pub struct FasterAPI {}

#[pymethods]
impl FasterAPI {
    #[new]
    fn new() -> Self {
        FasterAPI {}
    }

    fn register_route(&self, path: String, func: Py<PyAny>, _method: String) {
        let mut routes = ROUTES.lock().unwrap();
        routes.insert(path, func);
    }

    // to start the Rust HTTP server
    fn serve(&self, py: Python, host: Option<String>, port: Option<u16>) -> PyResult<()> {
        let host = host.unwrap_or_else(|| "127.0.0.1".to_string());
        let port = port.unwrap_or(8000);

        Ok(())
    }

    // @app.get("/path")
    fn get(&self, path: String, py: Python<'_>) -> PyResult<Py<PyCFunction>> {
        let path_clone = path.clone();
        let f = move |args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>| -> PyResult<Py<PyAny>> {
            Python::attach(|py| {
                let func: Py<PyAny> = args.get_item(0)?.extract()?;
                {
                    let mut routes = ROUTES.lock().unwrap();
                    routes.insert(format!("GET{}", path_clone), func.clone_ref(py));
                }
                let g = move |args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>| -> PyResult<Py<PyAny>> {
                    Python::attach(|py| func.call(py, args, kwargs))
                };
                let wrapped_func: Py<PyCFunction> = PyCFunction::new_closure(py, None, None, g)?.unbind();
                Ok(wrapped_func.into())
            })
        };

        let bound_func: Py<PyCFunction> = PyCFunction::new_closure(py, None, None, f)?.unbind();
        Ok(bound_func)
    }
}

#[pymodule]
fn fasterapi(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<FasterAPI>()?;
    Ok(())
}