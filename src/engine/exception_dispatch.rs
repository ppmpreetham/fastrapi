//! The dispatch of errors to the exception handlers that the user registers.
//! The functions find the handler for an exception, a validation error or a status code.
//! They call the handler and convert its result into an HTTP response.

use axum::response::{IntoResponse, Response};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyInt, PyString};
use pyo3::{Py, intern};

use axum::http::StatusCode;
use fastrapi_http::exceptions::{PyHTTPException, PyRequestValidationError};
use fastrapi_http::request::{create_py_request, create_stub_request};
use fastrapi_http::request_input::RequestInput;
use fastrapi_http::response_convert::convert_auto_response;
use fastrapi_pyutil::json::json_to_py_object;
use fastrapi_routing::request_data::ValidationErrorDetails;
use fastrapi_runtime::py_bridge::{PyTaskFuture, request_loop, schedule_task};
use fastrapi_runtime::server_state::{exception_handlers, server_loop};
use parking_lot::Mutex;
use std::sync::Arc;

/// The result of an `async` exception handler that is still running.
///
/// It goes in the extensions of a fallback response. [`resolve_pending_handler`]
/// awaits it at the async boundary, so no Tokio worker blocks on the handler.
#[derive(Clone)]
struct PendingHandlerResponse(Arc<Mutex<Option<PyTaskFuture>>>);

/// What an exception handler gave back.
enum HandlerResult {
    Ready(Response),
    Pending(PyTaskFuture),
}

impl HandlerResult {
    /// The response to send now. A pending result rides on `fallback`, the
    /// response that a failed handler gives.
    fn into_response(self, mut fallback: Response) -> Response {
        match self {
            Self::Ready(response) => response,
            Self::Pending(future) => {
                fallback
                    .extensions_mut()
                    .insert(PendingHandlerResponse(Arc::new(Mutex::new(Some(future)))));
                fallback
            }
        }
    }
}

/// Awaits the `async` exception handler attached to `response`, if there is one.
///
/// If the handler fails, the function prints the error and keeps the fallback
/// response, the same as for a synchronous handler that fails.
pub(crate) async fn resolve_pending_handler(mut response: Response) -> Response {
    let Some(pending) = response.extensions_mut().remove::<PendingHandlerResponse>() else {
        return response;
    };
    let Some(future) = pending.0.lock().take() else {
        return response;
    };
    let result = future.await;
    Python::attach(|py| match result {
        Ok(value) => convert_auto_response(py, value.bind(py)),
        Err(err) => {
            tracing::error!("Exception handler itself failed:");
            err.print(py);
            response
        }
    })
}

pub(crate) fn dispatch_validation_handler(
    py: Python<'_>,
    mut response: Response,
    request_input: Option<&RequestInput<'_>>,
) -> Response {
    let Some(details) = response.extensions_mut().remove::<ValidationErrorDetails>() else {
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
        Ok(result) => match handler_response(py, result) {
            Some(result) => result.into_response(response),
            None => response,
        },
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
    let handler = if let Ok(http_error) = exc_value.cast::<PyHTTPException>()
        && let Ok(Some(handler)) = bound_registry.get_item(http_error.borrow().status_code)
    {
        handler
    } else {
        find_handler(py, bound_registry, err)?
    };

    let request = build_request_arg(py, request_input);
    match handler.call1((request, exc_value)) {
        Ok(result) => handler_response(py, result)
            .map(|result| result.into_response(StatusCode::INTERNAL_SERVER_ERROR.into_response())),
        Err(handler_err) => {
            tracing::error!("Exception handler itself failed:");
            handler_err.print(py);
            Some(StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}

fn handler_response(py: Python<'_>, result: Bound<'_, PyAny>) -> Option<HandlerResult> {
    if result.hasattr("__await__").unwrap_or(false) {
        let event_loop = request_loop().or_else(server_loop)?;
        let future = schedule_task(py, event_loop, result).ok()?;
        return Some(HandlerResult::Pending(future));
    }
    Some(HandlerResult::Ready(convert_auto_response(py, &result)))
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
        Ok(result) => handler_response(py, result)
            .map(|result| result.into_response(StatusCode::INTERNAL_SERVER_ERROR.into_response())),
        Err(err) => {
            err.print(py);
            Some(StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}
