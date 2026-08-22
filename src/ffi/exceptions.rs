use std::borrow::Cow;

use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use pyo3::PyClassInitializer;
use pyo3::exceptions::{PyException, PyRuntimeError, PyUserWarning};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString, PyTuple};
use simd_json::json;

// Base Errors

#[pyclass(extends = PyRuntimeError, subclass, name = "FastAPIError")]
pub struct PyFastAPIError;

#[pymethods]
impl PyFastAPIError {
    #[new]
    fn new() -> Self {
        Self
    }
}

pub type PyFastrAPIError = PyFastAPIError;

#[pyclass(extends = PyFastAPIError, subclass, name = "DependencyScopeError")]
pub struct PyDependencyScopeError;

#[pymethods]
impl PyDependencyScopeError {
    #[new]
    fn new() -> PyClassInitializer<Self> {
        PyClassInitializer::from(PyFastAPIError).add_subclass(Self)
    }
}

#[pyclass(extends = PyFastAPIError, subclass, name = "PydanticV1NotSupportedError")]
pub struct PyPydanticV1NotSupportedError;

#[pymethods]
impl PyPydanticV1NotSupportedError {
    #[new]
    fn new() -> PyClassInitializer<Self> {
        PyClassInitializer::from(PyFastAPIError).add_subclass(Self)
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
        let len = errors
            .cast::<pyo3::types::PyList>()
            .map(|list| list.len())
            .or_else(|_| errors.cast::<pyo3::types::PyDict>().map(|dict| dict.len()))
            .unwrap_or(0);
        format!(
            "{} validation error{} occurred",
            len,
            if len == 1 { "" } else { "s" }
        )
    }
}

// Request/Response Validation Errors

crate::define_validation_subclass!(PyRequestValidationError, "RequestValidationError");
crate::define_validation_subclass!(
    PyWebSocketRequestValidationError,
    "WebSocketRequestValidationError"
);
crate::define_validation_subclass!(PyResponseValidationError, "ResponseValidationError");

// HTTP Exceptions

#[pyclass(
    extends = PyException,
    name = "HTTPException",
    get_all,
    from_py_object
)]
#[derive(Clone)]
pub struct PyHTTPException {
    pub status_code: u16,
    pub detail: Py<PyAny>,
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

fn response_headers(py: Python<'_>, headers: Option<&Py<PyDict>>) -> HeaderMap {
    let mut map = HeaderMap::new();
    let Some(headers) = headers else { return map };

    for (name, value) in headers.bind(py).iter() {
        let Ok(name) = name.extract::<Cow<'_, str>>() else {
            continue;
        };
        let Ok(value) = value.extract::<Cow<'_, str>>() else {
            continue;
        };
        let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        let Ok(value) = HeaderValue::from_str(&value) else {
            continue;
        };
        map.append(name, value);
    }
    map
}

fn body_allowed(status: StatusCode) -> bool {
    let code = status.as_u16();
    code >= 200 && !matches!(code, 204 | 205 | 304)
}

impl PyHTTPException {
    pub(crate) fn bad_request(py: Python<'_>, detail: &str) -> PyErr {
        let exc = Self {
            status_code: 400,
            detail: PyString::new(py, detail).into_any().unbind(),
            headers: None,
        };
        match Py::new(py, exc).map(Py::into_any) {
            Ok(instance) => PyErr::from_value(instance.into_bound(py)),
            Err(err) => err,
        }
    }

    pub fn to_response(&self, py: Python<'_>) -> Response {
        let status =
            StatusCode::try_from(self.status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let headers = response_headers(py, self.headers.as_ref());

        if !body_allowed(status) {
            return (status, headers).into_response();
        }

        let detail = crate::utils::py_any_to_json(py, self.detail.bind(py));
        (status, headers, Json(json!({ "detail": detail }))).into_response()
    }
}

// WebSocket Exceptions

#[pyclass(
    extends = PyException,
    name = "WebSocketException",
    get_all,
    from_py_object
)]
#[derive(Clone)]
pub struct PyWebSocketException {
    pub code: u16,
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

#[pyclass(extends = PyUserWarning, name = "FastAPIDeprecationWarning")]
pub struct PyFastAPIDeprecationWarning;

#[pymethods]
impl PyFastAPIDeprecationWarning {
    #[new]
    fn new() -> Self {
        Self
    }
}

pub type PyFastrAPIDeprecationWarning = PyFastAPIDeprecationWarning;
