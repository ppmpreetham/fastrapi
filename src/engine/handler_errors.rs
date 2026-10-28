//! The conversion of Python errors from route handlers into HTTP responses.
//! This module also writes Python errors to the log.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use fastrapi_http::exceptions::PyHTTPException;
use fastrapi_http::request_input::RequestInput;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use tracing::error;

use crate::engine::exception_dispatch::dispatch_exception_handler;

fastrapi_pyutil::cached_py_import!(TRACEBACK_MODULE, "traceback");

#[expect(
    clippy::needless_pass_by_value,
    reason = "convenience wrapper accepting owned PyErr"
)]
pub(crate) fn python_error_to_response(py: Python<'_>, err: PyErr) -> Response {
    python_error_to_response_in(py, &err, None)
}

/// Converts a Python error into a response with the handlers of the `@app.exception_handler` registry.
pub(crate) fn python_error_to_response_in(
    py: Python<'_>,
    err: &PyErr,
    request_input: Option<&RequestInput<'_>>,
) -> Response {
    if let Some(response) = dispatch_exception_handler(py, err, request_input) {
        return response;
    }

    if let Ok(http_error) = err.value(py).cast::<PyHTTPException>() {
        return http_error.borrow().to_response(py);
    }
    if let Some(http_error) = starlette_http_exception(py, err.value(py)) {
        return http_error.to_response(py);
    }
    log_python_error(py, err);
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

fn starlette_http_exception(py: Python<'_>, value: &Bound<'_, PyAny>) -> Option<PyHTTPException> {
    let class = STARLETTE_EXCEPTIONS
        .get(py)
        .ok()?
        .getattr(intern!(py, "HTTPException"))
        .ok()?;
    if !value.is_instance(&class).ok()? {
        return None;
    }
    let headers = value
        .getattr(intern!(py, "headers"))
        .ok()
        .and_then(|headers| headers.cast_into::<PyDict>().ok())
        .map(Bound::unbind);
    Some(PyHTTPException {
        status_code: value
            .getattr(intern!(py, "status_code"))
            .ok()?
            .extract()
            .ok()?,
        detail: value.getattr(intern!(py, "detail")).ok()?.unbind(),
        headers,
    })
}

fastrapi_pyutil::cached_py_import!(STARLETTE_EXCEPTIONS, "starlette.exceptions");

pub(crate) fn log_python_error(py: Python<'_>, err: &PyErr) {
    let rendered = (|| -> PyResult<String> {
        let traceback_bound = TRACEBACK_MODULE.get(py)?;

        let traceback_obj = err
            .traceback(py)
            .map_or_else(|| py.None(), |tb| tb.into_any().unbind());
        let lines = traceback_bound.call_method1(
            intern!(py, "format_exception"),
            (err.get_type(py), err.value(py), traceback_obj.bind(py)),
        )?;
        Ok(lines.extract::<Vec<String>>()?.concat())
    })();

    match rendered {
        Ok(traceback) => error!(target: "fastrapi::python", "Python handler error:\n{}", traceback),
        Err(format_err) => error!(
            target: "fastrapi::python",
            "Python handler error: {}; traceback formatting failed: {}", err, format_err
        ),
    }
}
