use axum::response::{IntoResponse, Response};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::http::{
    request::{create_py_request, create_stub_request},
    responses::convert_auto_response,
};
use crate::routing::types::RequestInput;
use axum::http::StatusCode;
use crate::ffi::exceptions::PyHTTPException;
use crate::ffi::exceptions::PyRequestValidationError;
use crate::ffi::pydantic::ValidationErrorDetails;
use crate::globals::PYTHON_RUNTIME;
use crate::globals::async_loop_pool;
use crate::globals::exception_handlers;
use crate::runtime::py_bridge::request_loop;
use crate::runtime::py_bridge::schedule_task;
use crate::utils::json_to_py_object;
use pyo3::Py;
use pyo3::types::PyInt;
use pyo3::types::PyString;
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio::task::block_in_place;

pub(crate) fn dispatch_validation_handler(
    py: Python<'_>,
    mut response: Response,
    request_input: Option<&RequestInput<'_>>,
) -> Response {
    let Some(details) = response
        .extensions_mut()
        .remove::<ValidationErrorDetails>()
    else {
        return response;
    };
    let Some(registry) = exception_handlers() else {
        return response;
    };
    let registry = registry.bind(py);
    let validation_type = py.get_type::<PyRequestValidationError>();
    let Ok(Some(handler)) = registry.get_item(&validation_type) else {
        return response;
    };
    let details = json_to_py_object(py, &details.0);
    let Ok(errors) = details.bind(py).get_item("detail") else {
        return response;
    };
    let Ok(exception) = validation_type.call1((errors,)) else {
        return response;
    };
    let request = build_request_arg(py, request_input);
    match handler.call1((request, exception)) {
        Ok(result) => handler_response(py, result).unwrap_or(response),
        Err(err) => {
            err.print(py);
            response
        }
    }
}

pub(crate) fn dispatch_exception_handler(
    py: Python<'_>,
    err: &PyErr,
    request_input: Option<&RequestInput<'_>>,
) -> Option<Response> {
    let registry = exception_handlers()?;
    let bound_registry = registry.bind(py);

    let exc_value = err.value(py).clone();
    let handler = if let Ok(http_error) =
        exc_value.cast::<PyHTTPException>()
        && let Ok(Some(handler)) = bound_registry.get_item(http_error.borrow().status_code)
    {
        handler
    } else {
        find_handler(py, bound_registry, err)?
    };

    let request = build_request_arg(py, request_input);
    match handler.call1((request, exc_value)) {
        Ok(result) => handler_response(py, result),
        Err(handler_err) => {
            tracing::error!("Exception handler itself failed:");
            handler_err.print(py);
            Some(StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}

fn handler_response(py: Python<'_>, result: Bound<'_, PyAny>) -> Option<Response> {
    if result.hasattr("__await__").unwrap_or(false) {
        let loop_ = request_loop().or_else(|| {
            async_loop_pool().map(|pool| Arc::new(pool.pick(py)))
        })?;
        let future = schedule_task(py, &loop_, result).ok()?;
        let result = py
            .detach(|| {
                if let Ok(handle) = Handle::try_current() {
                    block_in_place(|| handle.block_on(future))
                } else {
                    PYTHON_RUNTIME.block_on(future)
                }
            })
            .ok()?;
        return Some(convert_auto_response(py, result.bind(py)));
    }
    Some(convert_auto_response(py, &result))
}

fn find_handler<'py>(
    py: Python<'py>,
    registry: &Bound<'py, PyDict>,
    err: &PyErr,
) -> Option<Bound<'py, PyAny>> {
    let ty = err.get_type(py);
    let Ok(mro) = ty.getattr(intern!(py, "__mro__")) else {
        return None;
    };

    mro.try_iter().ok()?.find_map(|entry| {
        let entry = entry.ok()?;
        registry.get_item(entry).ok().flatten()
    })
}

fn build_request_arg(py: Python<'_>, request_input: Option<&RequestInput<'_>>) -> Py<PyAny> {
    request_input
        .and_then(|input| create_py_request(py, input, None).ok())
        .or_else(|| create_stub_request(py).ok())
        .unwrap_or_else(|| py.None())
}

pub(crate) fn dispatch_status_handler(py: Python<'_>, status: u16) -> Option<Response> {
    let registry = exception_handlers()?;
    let key = PyInt::new(py, status);
    let handler = registry.bind(py).get_item(key).ok().flatten()?;

    let exc = Py::new(
        py,
        PyHTTPException {
            status_code: status,
            detail: PyString::new(
                py,
                StatusCode::from_u16(status)
                    .ok()
                    .and_then(|status| status.canonical_reason())
                    .unwrap_or("Internal Server Error"),
            )
            .into_any()
            .unbind(),
            headers: None,
        },
    )
    .ok()?
    .into_any();
    let request = create_stub_request(py).ok()?;

    match handler.call1((request, exc.bind(py))) {
        Ok(result) => handler_response(py, result),
        Err(err) => {
            err.print(py);
            Some(StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}
