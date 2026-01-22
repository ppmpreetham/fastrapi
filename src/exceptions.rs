use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use pyo3::exceptions::{PyException, PyRuntimeError, PyUserWarning};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::json;

// --- Base Errors ---

#[pyclass(extends=PyRuntimeError, name = "FastrAPIError")]
pub struct PyFastrAPIError;

#[pymethods]
impl PyFastrAPIError {
    #[new]
    fn new() -> Self {
        Self
    }
}

#[pyclass(extends=PyException, subclass, name = "ValidationException")]
pub struct PyValidationException {
    #[pyo3(get)]
    pub _errors: Py<PyAny>,
}

#[pymethods]
impl PyValidationException {
    #[new]
    fn new(_errors: Py<PyAny>) -> Self {
        Self { _errors }
    }

    fn errors(&self) -> Py<PyAny> {
        self._errors.clone()
    }

    fn __str__(&self, py: Python<'_>) -> String {
        let errors = self._errors.bind(py);
        let len = errors.len().unwrap_or(0);
        format!(
            "{} validation error{} occurred",
            len,
            if len == 1 { "" } else { "s" }
        )
    }
}

// --- Request/Response Validation Errors ---

#[pyclass(extends=PyValidationException, name = "RequestValidationError")]
pub struct PyRequestValidationError {
    #[pyo3(get)]
    pub body: Py<PyAny>,
}

#[pymethods]
impl PyRequestValidationError {
    #[new]
    #[pyo3(signature = (errors, *, body=None))]
    fn new(errors: Py<PyAny>, body: Option<Py<PyAny>>) -> (Self, PyValidationException) {
        let body = body.unwrap_or_else(|| Python::attach(|py| py.None()));
        (Self { body }, PyValidationException::new(errors))
    }
}

#[pyclass(extends=PyValidationException, name = "ResponseValidationError")]
pub struct PyResponseValidationError {
    #[pyo3(get)]
    pub body: Py<PyAny>,
}

#[pymethods]
impl PyResponseValidationError {
    #[new]
    #[pyo3(signature = (errors, *, body=None))]
    fn new(errors: Py<PyAny>, body: Option<Py<PyAny>>) -> (Self, PyValidationException) {
        let body = body.unwrap_or_else(|| Python::attach(|py| py.None()));
        (Self { body }, PyValidationException::new(errors))
    }
}

// --- HTTP Exceptions ---

#[pyclass(extends=PyException, name = "HTTPException")]
#[derive(Clone)]
pub struct PyHTTPException {
    #[pyo3(get)]
    pub status_code: u16,
    #[pyo3(get)]
    pub detail: Py<PyAny>,
    #[pyo3(get)]
    pub headers: Option<Py<PyDict>>,
}

#[pymethods]
impl PyHTTPException {
    #[new]
    #[pyo3(signature = (status_code, detail=None, headers=None))]
    fn new(
        py: Python<'_>,
        status_code: u16,
        detail: Option<Py<PyAny>>,
        headers: Option<Py<PyDict>>,
    ) -> Self {
        let detail = detail.unwrap_or_else(|| py.None());
        Self {
            status_code,
            detail,
            headers,
        }
    }

    fn __str__(&self, py: Python<'_>) -> String {
        let d = self.detail.bind(py);
        let s = d
            .str()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        format!("{}: {}", self.status_code, s)
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        let d = self.detail.bind(py);
        let r = d
            .repr()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        format!(
            "HTTPException(status_code={}, detail={})",
            self.status_code, r
        )
    }
}

impl PyHTTPException {
    pub fn to_response(&self, py: Python<'_>) -> Response {
        let status =
            StatusCode::from_u16(self.status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let detail_json = crate::utils::py_any_to_json(py, self.detail.bind(py));
        (status, Json(json!({ "detail": detail_json }))).into_response()
    }
}

// --- WebSocket Exceptions ---

#[pyclass(extends=PyException, name = "WebSocketException")]
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

    fn __str__(&self) -> String {
        format!("{}: {}", self.code, self.reason.as_deref().unwrap_or(""))
    }

    fn __repr__(&self) -> String {
        format!(
            "WebSocketException(code={}, reason={:?})",
            self.code, self.reason
        )
    }
}

#[pyclass(extends=PyUserWarning, name = "FastrAPIDeprecationWarning")]
pub struct PyFastrAPIDeprecationWarning;

#[pymethods]
impl PyFastrAPIDeprecationWarning {
    #[new]
    fn new() -> Self {
        Self
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyFastrAPIError>()?;
    m.add_class::<PyValidationException>()?;
    m.add_class::<PyRequestValidationError>()?;
    m.add_class::<PyResponseValidationError>()?;
    m.add_class::<PyHTTPException>()?;
    m.add_class::<PyWebSocketException>()?;
    m.add_class::<PyFastrAPIDeprecationWarning>()?;
    Ok(())
}
