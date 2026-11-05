//! The conversion of Python errors from route handlers into HTTP responses.
//! This module also writes Python errors to the log.

use axum::response::Response;
use fastrapi_http::exceptions::{http_exception_response, validation_error_response};
use fastrapi_http::request_input::RequestInput;
use fastrapi_runtime::py_bridge::EventLoop;
use pyo3::intern;
use pyo3::prelude::*;
use tracing::error;

use crate::engine::exception_dispatch::{dispatch_exception_handler, internal_server_error};

fastrapi_pyutil::cached_py_import!(TRACEBACK_MODULE, "traceback");

#[expect(
    clippy::needless_pass_by_value,
    reason = "convenience wrapper accepting owned PyErr"
)]
pub(crate) fn python_error_to_response(py: Python<'_>, err: PyErr, server: EventLoop) -> Response {
    python_error_response(py, &err, None, Some(server))
}

pub(crate) fn error_response(
    py: Python<'_>,
    err: &PyErr,
    request_input: Option<&RequestInput<'_>>,
    server: EventLoop,
) -> Response {
    python_error_response(py, err, request_input, Some(server))
}

/// Converts a Python error into a response with the handlers of the `@app.exception_handler` registry.
pub(crate) fn python_error_to_response_in(
    py: Python<'_>,
    err: &PyErr,
    request_input: Option<&RequestInput<'_>>,
) -> Response {
    python_error_response(py, err, request_input, None)
}

fn python_error_response(
    py: Python<'_>,
    err: &PyErr,
    request_input: Option<&RequestInput<'_>>,
    server: Option<EventLoop>,
) -> Response {
    if let Some(response) = dispatch_exception_handler(py, err, request_input, server) {
        return response;
    }

    let value = err.value(py);
    if let Some(response) =
        http_exception_response(value).or_else(|| validation_error_response(value))
    {
        return response;
    }
    log_python_error(py, err);
    internal_server_error()
}

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
