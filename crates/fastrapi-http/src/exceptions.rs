//! This module contains the Python exception and warning classes of fastrapi.
//! These classes are compatible with the FastAPI and starlette exception classes.

use std::borrow::Cow;

use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use fastrapi_pyutil::json::py_any_to_json;
use pyo3::PyClassInitializer;
use pyo3::exceptions::{PyException, PyRuntimeError, PyUserWarning};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyDictMethods, PyList, PyListMethods, PyString, PyTuple};
use simd_json::json;

/// A generic, FastAPI-specific error.
#[pyclass(extends = PyRuntimeError, subclass, name = "FastAPIError")]
pub struct PyFastAPIError;

#[pymethods]
impl PyFastAPIError {
    #[new]
    const fn new() -> Self {
        Self
    }
}

pub type PyFastrAPIError = PyFastAPIError;

/// A dependency declared that it depends on another dependency with an invalid
/// (narrower) scope.
#[pyclass(extends = PyFastAPIError, subclass, name = "DependencyScopeError")]
pub struct PyDependencyScopeError;

#[pymethods]
impl PyDependencyScopeError {
    #[new]
    fn new() -> PyClassInitializer<Self> {
        PyClassInitializer::from(PyFastAPIError).add_subclass(Self)
    }
}

/// A pydantic.v1 model is used, which is no longer supported.
#[pyclass(extends = PyFastAPIError, subclass, name = "PydanticV1NotSupportedError")]
pub struct PyPydanticV1NotSupportedError;

#[pymethods]
impl PyPydanticV1NotSupportedError {
    #[new]
    fn new() -> PyClassInitializer<Self> {
        PyClassInitializer::from(PyFastAPIError).add_subclass(Self)
    }
}

/// Common base class for all non-exit exceptions.
#[pyclass(extends = PyException, subclass, name = "ValidationException")]
pub struct PyValidationException {
    #[expect(
        clippy::pub_underscore_fields,
        reason = "FastAPI RequestValidationError._errors attribute compatibility"
    )]
    #[pyo3(get)]
    pub _errors: Py<PyAny>,
    #[pyo3(get)]
    pub endpoint_ctx: Option<Py<PyAny>>,
}

impl PyValidationException {
    fn context_value(&self, py: Python<'_>, key: &str) -> Option<Py<PyAny>> {
        let ctx = self.endpoint_ctx.as_ref()?.bind(py);
        ctx.call_method1(pyo3::intern!(py, "get"), (key,))
            .ok()
            .filter(|value| !value.is_none())
            .map(Bound::unbind)
    }
}

#[pymethods]
impl PyValidationException {
    #[new]
    #[pyo3(signature = (errors, *, endpoint_ctx=None))]
    pub fn new(errors: Bound<'_, PyAny>, endpoint_ctx: Option<Bound<'_, PyAny>>) -> Self {
        Self {
            _errors: errors.into(),
            endpoint_ctx: endpoint_ctx.filter(|ctx| !ctx.is_none()).map(Bound::unbind),
        }
    }

    #[getter]
    fn endpoint_function(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        self.context_value(py, "function")
    }

    #[getter]
    fn endpoint_path(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        self.context_value(py, "path")
    }

    #[getter]
    fn endpoint_file(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        self.context_value(py, "file")
    }

    #[getter]
    fn endpoint_line(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        self.context_value(py, "line")
    }

    #[pyo3(signature = (*_args, **_kwargs))]
    const fn __init__(&self, _args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) {}

    #[expect(
        clippy::used_underscore_binding,
        reason = "Accesses Python compatibility field _errors"
    )]
    fn errors<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self._errors.bind(py).clone()
    }

    #[expect(
        clippy::used_underscore_binding,
        reason = "Accesses Python compatibility field _errors"
    )]
    fn __str__(&self, py: Python<'_>) -> String {
        let errors = self._errors.bind(py);
        let len = errors
            .cast::<PyList>()
            .map(PyListMethods::len)
            .or_else(|_| errors.cast::<PyDict>().map(PyDictMethods::len))
            .unwrap_or(0);
        format!(
            "{} validation error{} occurred",
            len,
            if len == 1 { "" } else { "s" }
        )
    }
}

crate::macros::define_validation_subclass!(
    /// Common base class for all non-exit exceptions.
    PyRequestValidationError, "RequestValidationError");
crate::macros::define_validation_subclass!(
    PyWebSocketRequestValidationError,
    "WebSocketRequestValidationError"
);
crate::macros::define_validation_subclass!(
    /// Common base class for all non-exit exceptions.
    PyResponseValidationError, "ResponseValidationError");

/// An HTTP exception you can raise in your own code to show errors to the client.
///
/// This is for client errors, invalid authentication, invalid data, etc. Not for server
/// errors in your code.
///
/// Read more about it in the
/// [FastAPI docs for Handling Errors](https://fastapi.tiangolo.com/tutorial/handling-errors/).
///
/// ## Example
///
/// ```python
/// from fastrapi import FastAPI, HTTPException
///
/// app = FastAPI()
///
/// items = {"foo": "The Foo Wrestlers"}
///
///
/// @app.get("/items/{item_id}")
/// async def read_item(item_id: str):
///     if item_id not in items:
///         raise HTTPException(status_code=404, detail="Item not found")
///     return {"item": items[item_id]}
/// ```
#[pyclass(
    extends = PyException,
    subclass,
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
    #[pyo3(signature = (*args, **kwargs))]
    fn new(py: Python<'_>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        let argument = |index: usize, name: &str| {
            args.get_item(index)
                .ok()
                .or_else(|| kwargs.and_then(|kwargs| kwargs.get_item(name).ok().flatten()))
        };
        let status_code = argument(0, "status_code")
            .and_then(|code| code.extract().ok())
            .unwrap_or(500);
        let detail = argument(1, "detail").filter(|detail| !detail.is_none());
        let headers = argument(2, "headers").and_then(|headers| headers.cast_into().ok());
        Self::with_detail(py, status_code, detail, headers)
    }

    #[pyo3(signature = (status_code, detail=None, headers=None))]
    fn __init__(
        &mut self,
        py: Python<'_>,
        status_code: u16,
        detail: Option<Bound<'_, PyAny>>,
        headers: Option<Bound<'_, PyDict>>,
    ) {
        *self = Self::with_detail(py, status_code, detail, headers);
    }

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

pub fn status_phrase(py: Python<'_>, status_code: u16) -> Py<PyAny> {
    py.import("http")
        .and_then(|http| {
            http.getattr(pyo3::intern!(py, "HTTPStatus"))?
                .call1((status_code,))
        })
        .and_then(|status| status.getattr(pyo3::intern!(py, "phrase")))
        .map_or_else(
            |_| {
                let phrase = StatusCode::from_u16(status_code)
                    .ok()
                    .and_then(|status| status.canonical_reason())
                    .unwrap_or("Internal Server Error");
                PyString::new(py, phrase).into_any().unbind()
            },
            Bound::unbind,
        )
}

const fn body_allowed(status: StatusCode) -> bool {
    let code = status.as_u16();
    code >= 200 && !matches!(code, 204 | 205 | 304)
}

impl PyHTTPException {
    fn with_detail(
        py: Python<'_>,
        status_code: u16,
        detail: Option<Bound<'_, PyAny>>,
        headers: Option<Bound<'_, PyDict>>,
    ) -> Self {
        let detail = match detail {
            Some(detail) => detail.unbind(),
            None => status_phrase(py, status_code),
        };
        Self {
            status_code,
            detail,
            headers: headers.map(Into::into),
        }
    }

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

    #[must_use]
    pub fn to_response(&self, py: Python<'_>) -> Response {
        let status =
            StatusCode::try_from(self.status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let headers = response_headers(py, self.headers.as_ref());

        if !body_allowed(status) {
            return (status, headers).into_response();
        }

        let detail = py_any_to_json(py, self.detail.bind(py));
        (status, headers, Json(json!({ "detail": detail }))).into_response()
    }
}

/// A WebSocket exception you can raise in your own code to show errors to the client.
///
/// This is for client errors, invalid authentication, invalid data, etc. Not for server
/// errors in your code.
///
/// Read more about it in the
/// [FastAPI docs for WebSockets](https://fastapi.tiangolo.com/advanced/websockets/).
///
/// ## Example
///
/// ```python
/// from typing import Annotated
///
/// from fastrapi import (
///     Cookie,
///     FastAPI,
///     WebSocket,
///     WebSocketException,
///     status,
/// )
///
/// app = FastAPI()
///
/// @app.websocket("/items/{item_id}/ws")
/// async def websocket_endpoint(
///     *,
///     websocket: WebSocket,
///     session: Annotated[str | None, Cookie()] = None,
///     item_id: str,
/// ):
///     if session is None:
///         raise WebSocketException(code=status.WS_1008_POLICY_VIOLATION)
///     await websocket.accept()
///     while True:
///         data = await websocket.receive_text()
///         await websocket.send_text(f"Session cookie is: {session}")
///         await websocket.send_text(f"Message text was: {data}, for item ID: {item_id}")
/// ```
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

/// Common base class for all non-exit exceptions.
#[pyclass(
    extends = PyException,
    name = "WebSocketDisconnect",
    get_all,
    from_py_object
)]
#[derive(Clone)]
pub struct PyWebSocketDisconnect {
    pub code: u16,
    pub reason: Option<String>,
}

#[pymethods]
impl PyWebSocketDisconnect {
    #[new]
    #[pyo3(signature = (code=1000, reason=None))]
    const fn new(code: u16, reason: Option<String>) -> Self {
        Self { code, reason }
    }

    #[pyo3(signature = (*_args, **_kwargs))]
    const fn __init__(&self, _args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) {}
}

#[pymethods]
impl PyWebSocketException {
    #[new]
    #[pyo3(signature = (code, reason=None))]
    const fn new(code: u16, reason: Option<String>) -> Self {
        Self { code, reason }
    }

    #[pyo3(signature = (*_args, **_kwargs))]
    const fn __init__(&self, _args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) {}

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

/// A custom deprecation warning as DeprecationWarning is ignored
/// Ref: https://sethmlarson.dev/deprecations-via-warnings-dont-work-for-python-libraries
#[pyclass(extends = PyUserWarning, name = "FastAPIDeprecationWarning")]
pub struct PyFastAPIDeprecationWarning;

#[pymethods]
impl PyFastAPIDeprecationWarning {
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    const fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        Self
    }
}

pub type PyFastrAPIDeprecationWarning = PyFastAPIDeprecationWarning;
