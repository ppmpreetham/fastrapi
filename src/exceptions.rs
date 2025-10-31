// HTTPException(status_code, detail=None, headers=None)

use pyo3::{pyclass, pymethods, types::PyDict, Py};

#[pyclass(name = "HTTPException", extends = pyo3::exceptions::PyException)]
#[derive(Clone)]
pub struct PyHTTPException {
    #[pyo3(get)]
    pub status_code: u16,
    #[pyo3(get)]
    pub detail: Option<String>,
    #[pyo3(get)]
    pub headers: Option<Py<PyDict>>,
}

#[pymethods]
impl PyHTTPException {
    #[new]
    #[pyo3(signature = (status_code, detail=None, headers=None))]
    fn new(status_code: u16, detail: Option<String>, headers: Option<Py<PyDict>>) -> Self {
        Self {
            status_code,
            detail,
            headers,
        }
    }
}

// WebSocketException(code, reason=None)

#[pyclass(name = "WebSocketException", extends = pyo3::exceptions::PyException)]
#[derive(Clone)]
pub struct PyWebSocketException {
    #[pyo3(get)]
    pub code: u16,
    #[pyo3(get)]
    pub reason: Option<String>,
}

#[pymethods]
impl PyWebSocketException {
    #[new]
    #[pyo3(signature = (code, reason=None))]
    fn new(code: u16, reason: Option<String>) -> Self {
        Self { code, reason }
    }
}
