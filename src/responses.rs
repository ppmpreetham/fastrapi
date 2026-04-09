use pyo3::prelude::*;
use pyo3::{pyclass, pymethods, Py, PyAny};

// wrapper classes
#[pyclass(name = "HTMLResponse", skip_from_py_object)]
#[derive(Clone)]
pub struct PyHTMLResponse {
    #[pyo3(get)]
    pub content: String,
    #[pyo3(get)]
    pub status_code: u16,
}

#[pymethods]
impl PyHTMLResponse {
    #[new]
    #[pyo3(signature = (content, status_code=200))]
    fn new(content: String, status_code: u16) -> Self {
        Self {
            content,
            status_code,
        }
    }
}

#[pyclass(name = "JSONResponse", skip_from_py_object)]
#[derive(Clone)]
pub struct PyJSONResponse {
    #[pyo3(get)]
    pub content: Py<PyAny>,
    #[pyo3(get)]
    pub status_code: u16,
}

#[pymethods]
impl PyJSONResponse {
    #[new]
    #[pyo3(signature = (content, status_code=200))]
    fn new(content: Py<PyAny>, status_code: u16) -> Self {
        Self {
            content,
            status_code,
        }
    }
}

#[pyclass(name = "PlainTextResponse", skip_from_py_object)]
#[derive(Clone)]
pub struct PyPlainTextResponse {
    #[pyo3(get)]
    pub content: String,
    #[pyo3(get)]
    pub status_code: u16,
}

#[pymethods]
impl PyPlainTextResponse {
    #[new]
    #[pyo3(signature = (content, status_code=200))]
    fn new(content: String, status_code: u16) -> Self {
        Self {
            content,
            status_code,
        }
    }
}

#[pyclass(name = "RedirectResponse", skip_from_py_object)]
#[derive(Clone)]
pub struct PyRedirectResponse {
    #[pyo3(get)]
    pub url: String,
    #[pyo3(get)]
    pub status_code: u16,
}

#[pymethods]
impl PyRedirectResponse {
    #[new]
    #[pyo3(signature = (url, status_code=307))]
    fn new(url: String, status_code: u16) -> Self {
        Self { url, status_code }
    }
}

pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    let responses_module = PyModule::new(py, "responses")?;

    responses_module.add_class::<PyJSONResponse>()?;
    responses_module.add_class::<PyHTMLResponse>()?;
    responses_module.add_class::<PyPlainTextResponse>()?;
    responses_module.add_class::<PyRedirectResponse>()?;

    parent.add_submodule(&responses_module)?;

    let parent_name: String = parent.getattr("__name__")?.extract()?;
    let base_name = if parent_name.ends_with(".fastrapi") {
        parent_name.strip_suffix(".fastrapi").unwrap().to_string()
    } else {
        parent_name
    };
    let full_name = format!("{}.responses", base_name);
    responses_module.setattr("__name__", &full_name)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item(&full_name, &responses_module)?;
    Ok(())
}
