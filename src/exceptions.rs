use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use pyo3::exceptions::{PyException, PyRuntimeError, PyUserWarning};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};
use serde_json::json;

// Base Errors

#[pyclass(extends = PyRuntimeError, name = "FastrAPIError")]
pub struct PyFastrAPIError;

#[pymethods]
impl PyFastrAPIError {
    #[new]
    fn new() -> Self {
        Self
    }
}

#[pyclass(extends = PyException, subclass, name = "ValidationException")]
pub struct PyValidationException {
    #[pyo3(get)]
    pub _errors: Py<PyAny>,
}

#[pymethods]
impl PyValidationException {
    #[new]
    fn new(errors: Bound<'_, PyAny>) -> Self {
        Self {
            _errors: errors.into(),
        }
    }

    #[pyo3(signature = (*_args, **_kwargs))]
    fn __init__(&self, _args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) {}

    fn errors<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self._errors.bind(py).clone()
    }

    fn __str__(&self, py: Python<'_>) -> String {
        let errors = self._errors.bind(py);
        let len = if let Ok(list) = errors.cast::<pyo3::types::PyList>() {
            list.len()
        } else if let Ok(dict) = errors.cast::<pyo3::types::PyDict>() {
            dict.len()
        } else {
            0
        };
        format!(
            "{} validation error{} occurred",
            len,
            if len == 1 { "" } else { "s" }
        )
    }
}

// Request/Response Validation Errors

#[pyclass(extends = PyValidationException, name = "RequestValidationError")]
pub struct PyRequestValidationError {
    #[pyo3(get)]
    pub body: Py<PyAny>,
}

#[pymethods]
impl PyRequestValidationError {
    #[new]
    #[pyo3(signature = (errors, *, body=None))]
    fn new(
        py: Python<'_>,
        errors: Bound<'_, PyAny>,
        body: Option<Bound<'_, PyAny>>,
    ) -> (Self, PyValidationException) {
        let body_py = body.map(|b| b.into()).unwrap_or_else(|| py.None());
        (
            Self { body: body_py },
            PyValidationException::new(errors.into()),
        )
    }

    #[pyo3(signature = (*_args, **_kwargs))]
    fn __init__(&self, _args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) {}
}

#[pyclass(extends = PyValidationException, name = "ResponseValidationError")]
pub struct PyResponseValidationError {
    #[pyo3(get)]
    pub body: Py<PyAny>,
}

#[pymethods]
impl PyResponseValidationError {
    #[new]
    #[pyo3(signature = (errors, *, body=None))]
    fn new(
        py: Python<'_>,
        errors: Bound<'_, PyAny>,
        body: Option<Bound<'_, PyAny>>,
    ) -> (Self, PyValidationException) {
        let body_py = body.map(|b| b.into()).unwrap_or_else(|| py.None());
        (
            Self { body: body_py },
            PyValidationException::new(errors.into()),
        )
    }

    #[pyo3(signature = (*_args, **_kwargs))]
    fn __init__(&self, _args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) {}
}

// HTTP Exceptions

#[pyclass(extends = PyException, name = "HTTPException", skip_from_py_object)]
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
        detail: Option<Bound<'_, PyAny>>,
        headers: Option<Bound<'_, PyDict>>,
    ) -> Self {
        Self {
            status_code,
            detail: detail.map(|d| d.into()).unwrap_or_else(|| py.None()),
            headers: headers.map(|h| h.into()),
        }
    }

    #[pyo3(signature = (*_args, **_kwargs))]
    fn __init__(&self, _args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) {}

    fn __str__(&self, py: Python<'_>) -> String {
        let detail = self.detail.bind(py);
        format!("{}: {}", self.status_code, detail)
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        let r = self
            .detail
            .bind(py)
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
            StatusCode::try_from(self.status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let detail_json = crate::utils::py_any_to_json(py, self.detail.bind(py));
        (status, Json(json!({ "detail": detail_json }))).into_response()
    }
}

// WebSocket Exceptions

#[pyclass(extends = PyException, name = "WebSocketException", skip_from_py_object)]
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

    #[pyo3(signature = (*_args, **_kwargs))]
    fn __init__(&self, _args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) {}

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

#[pyclass(extends = PyUserWarning, name = "FastrAPIDeprecationWarning")]
pub struct PyFastrAPIDeprecationWarning;

#[pymethods]
impl PyFastrAPIDeprecationWarning {
    #[new]
    fn new() -> Self {
        Self
    }
}
