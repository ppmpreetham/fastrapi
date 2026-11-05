//! This module contains the Python exception classes of fastrapi.
//! The HTTP and validation exceptions come from an embedded port of FastAPI's `exceptions.py`.

use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use fastrapi_pyutil::json::py_json_response_with_status;
use pyo3::exceptions::PyException;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyModule, PyString, PyTuple, PyType};

const EXCEPTIONS_SOURCE: &str = include_str!("py/exceptions.py");
const HANDLERS_SOURCE: &str = include_str!("py/exception_handlers.py");

static EXCEPTIONS: PyOnceLock<Py<PyModule>> = PyOnceLock::new();
static HTTP_EXCEPTION: PyOnceLock<Py<PyType>> = PyOnceLock::new();
static REQUEST_VALIDATION_ERROR: PyOnceLock<Py<PyType>> = PyOnceLock::new();
static WEBSOCKET_VALIDATION_ERROR: PyOnceLock<Py<PyType>> = PyOnceLock::new();
static DEPRECATION_WARNING: PyOnceLock<Py<PyType>> = PyOnceLock::new();

fastrapi_pyutil::cached_py_import!(
    STARLETTE_HTTP_EXCEPTION,
    "starlette.exceptions",
    "HTTPException"
);

pub fn exceptions_module(py: Python<'_>) -> PyResult<&Bound<'_, PyModule>> {
    EXCEPTIONS
        .get_or_try_init(py, || {
            let module = fastrapi_pyutil::imports::python_source_module(
                py,
                EXCEPTIONS_SOURCE,
                c"fastrapi/exceptions.py",
                c"fastrapi.exceptions",
            )?;
            module.add("WebSocketException", py.get_type::<PyWebSocketException>())?;
            module.add(
                "WebSocketDisconnect",
                py.get_type::<PyWebSocketDisconnect>(),
            )?;
            fastrapi_pyutil::imports::register_module(py, "fastrapi.exceptions", &module)?;
            Ok::<_, PyErr>(module.unbind())
        })
        .map(|module| module.bind(py))
}

pub fn exception_handlers_module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    let module = fastrapi_pyutil::imports::python_source_module(
        py,
        HANDLERS_SOURCE,
        c"fastrapi/exception_handlers.py",
        c"fastrapi.exception_handlers",
    )?;
    fastrapi_pyutil::imports::register_module(py, "fastrapi.exception_handlers", &module)?;
    Ok(module)
}

fn exception_type<'py>(
    py: Python<'py>,
    cell: &'static PyOnceLock<Py<PyType>>,
    name: &Bound<'py, PyString>,
) -> PyResult<&'py Bound<'py, PyType>> {
    cell.get_or_try_init(py, || {
        exceptions_module(py)?
            .getattr(name)?
            .cast_into::<PyType>()
            .map(Bound::unbind)
            .map_err(PyErr::from)
    })
    .map(|ty| ty.bind(py))
}

pub fn http_exception_type(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    exception_type(py, &HTTP_EXCEPTION, intern!(py, "HTTPException"))
}

pub fn request_validation_error_type(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    exception_type(
        py,
        &REQUEST_VALIDATION_ERROR,
        intern!(py, "RequestValidationError"),
    )
}

pub fn websocket_validation_error_type(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    exception_type(
        py,
        &WEBSOCKET_VALIDATION_ERROR,
        intern!(py, "WebSocketRequestValidationError"),
    )
}

pub fn deprecation_warning_type(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    exception_type(
        py,
        &DEPRECATION_WARNING,
        intern!(py, "FastAPIDeprecationWarning"),
    )
}

fn raised(result: PyResult<Bound<'_, PyAny>>) -> PyErr {
    match result {
        Ok(value) => PyErr::from_value(value),
        Err(err) => err,
    }
}

#[must_use]
pub fn http_exception(
    py: Python<'_>,
    status_code: u16,
    detail: Option<&str>,
    headers: Option<Bound<'_, PyDict>>,
) -> PyErr {
    raised(http_exception_type(py).and_then(|class| class.call1((status_code, detail, headers))))
}

#[must_use]
pub fn request_validation_error(
    py: Python<'_>,
    errors: Bound<'_, PyAny>,
    body: Option<Bound<'_, PyAny>>,
) -> PyErr {
    let kwargs = PyDict::new(py);
    raised(
        kwargs
            .set_item(intern!(py, "body"), body)
            .and_then(|()| request_validation_error_type(py))
            .and_then(|class| class.call((errors,), Some(&kwargs))),
    )
}

#[must_use]
pub fn websocket_validation_error(py: Python<'_>, errors: Bound<'_, PyAny>) -> PyErr {
    raised(websocket_validation_error_type(py).and_then(|class| class.call1((errors,))))
}

#[must_use]
pub fn is_http_exception(value: &Bound<'_, PyAny>) -> bool {
    STARLETTE_HTTP_EXCEPTION.is_instance(value.py(), value)
}

#[must_use]
pub fn http_status_code(value: &Bound<'_, PyAny>) -> Option<u16> {
    value
        .getattr(intern!(value.py(), "status_code"))
        .ok()?
        .extract()
        .ok()
}

fn response_headers(headers: &Bound<'_, PyAny>) -> HeaderMap {
    let py = headers.py();
    let mut map = HeaderMap::new();
    let Ok(items) = headers
        .call_method0(intern!(py, "items"))
        .and_then(|items| items.try_iter())
    else {
        return map;
    };
    for item in items.flatten() {
        let Ok((name, value)) = item.extract::<(String, String)>() else {
            continue;
        };
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            map.append(name, value);
        }
    }
    map
}

const fn body_allowed(status: StatusCode) -> bool {
    !matches!(status.as_u16(), 100..=199 | 204 | 304)
}

pub fn http_exception_response(value: &Bound<'_, PyAny>) -> Option<Response> {
    if !is_http_exception(value) {
        return None;
    }
    let py = value.py();
    let status = StatusCode::from_u16(http_status_code(value)?).ok()?;
    let headers = value
        .getattr(intern!(py, "headers"))
        .ok()
        .filter(|headers| !headers.is_none())
        .map(|headers| response_headers(&headers))
        .unwrap_or_default();
    if !body_allowed(status) {
        return Some((status, headers).into_response());
    }
    let detail = value.getattr(intern!(py, "detail")).ok()?;
    let content = PyDict::new(py);
    content.set_item(intern!(py, "detail"), detail).ok()?;
    let mut response = py_json_response_with_status(py, status, content.as_any()).ok()?;
    response.headers_mut().extend(headers);
    Some(response)
}

pub fn validation_error_response(value: &Bound<'_, PyAny>) -> Option<Response> {
    let py = value.py();
    if !value
        .is_instance(request_validation_error_type(py).ok()?)
        .ok()?
    {
        return None;
    }
    let content = PyDict::new(py);
    content
        .set_item(
            intern!(py, "detail"),
            value.call_method0(intern!(py, "errors")).ok()?,
        )
        .ok()?;
    py_json_response_with_status(py, StatusCode::UNPROCESSABLE_ENTITY, content.as_any()).ok()
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
