//! The dispatch of errors to the exception handlers that the user registers.
//! The functions find the handler for an exception, a validation error or a status code.
//! They call the handler and convert its result into an HTTP response.

use axum::response::{IntoResponse, Response};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyInt};
use pyo3::{Py, intern};

use axum::http::StatusCode;
use fastrapi_http::exceptions::{PyHTTPException, PyRequestValidationError, status_phrase};
use fastrapi_http::request::{create_py_request, create_stub_request};
use fastrapi_http::request_input::RequestInput;
use fastrapi_http::response_convert::convert_auto_response;
use fastrapi_pyutil::json::json_to_py_object;
use fastrapi_routing::request_data::ValidationErrorDetails;
use fastrapi_runtime::py_bridge::{EventLoop, PyTaskFuture, schedule_task};
use fastrapi_runtime::server_state::current_server;
use ahash::AHashMap;
use parking_lot::Mutex;
use std::sync::{Arc, LazyLock};

type HandlerSlot = (Py<PyAny>, usize, Option<Py<PyAny>>);

static HANDLER_CACHE: LazyLock<Mutex<AHashMap<(usize, usize), HandlerSlot>>> =
    LazyLock::new(|| Mutex::new(AHashMap::new()));

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
    let Some(server) = server_of(request_input) else {
        return response;
    };
    let Some(registry) = server.server().exception_handlers() else {
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
        Ok(result) => match handler_response(py, server, result) {
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
    server: Option<EventLoop>,
) -> Option<Response> {
    let server = server.or_else(|| server_of(request_input))?;
    let bound_registry = server.server().exception_handlers()?.bind(py);

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
        Ok(result) => handler_response(py, server, result)
            .map(|result| result.into_response(StatusCode::INTERNAL_SERVER_ERROR.into_response())),
        Err(handler_err) => {
            tracing::error!("Exception handler itself failed:");
            handler_err.print(py);
            Some(StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}

fn server_of(request_input: Option<&RequestInput<'_>>) -> Option<EventLoop> {
    request_input
        .and_then(|input| input.server)
        .or_else(current_server)
}

fn handler_response(
    py: Python<'_>,
    server: EventLoop,
    result: Bound<'_, PyAny>,
) -> Option<HandlerResult> {
    if result.hasattr(intern!(py, "__await__")).unwrap_or(false) {
        let future = schedule_task(py, server, result).ok()?;
        return Some(HandlerResult::Pending(future));
    }
    Some(HandlerResult::Ready(convert_auto_response(py, &result)))
}

pub(crate) fn find_handler<'py>(
    py: Python<'py>,
    registry: &Bound<'py, PyDict>,
    err: &PyErr,
) -> Option<Bound<'py, PyAny>> {
    let ty = err.get_type(py);
    let key = (registry.as_ptr() as usize, ty.as_ptr() as usize);
    let len = registry.len();
    if let Some((_, cached_len, handler)) = HANDLER_CACHE.lock().get(&key)
        && *cached_len == len
    {
        return handler.as_ref().map(|handler| handler.bind(py).clone());
    }
    let handler = lookup_handler(py, registry, &ty);
    HANDLER_CACHE.lock().insert(
        key,
        (
            ty.into_any().unbind(),
            len,
            handler.as_ref().map(|handler| handler.clone().unbind()),
        ),
    );
    handler
}

fn lookup_handler<'py>(
    py: Python<'py>,
    registry: &Bound<'py, PyDict>,
    ty: &Bound<'py, pyo3::types::PyType>,
) -> Option<Bound<'py, PyAny>> {
    let Ok(mro) = ty.getattr(intern!(py, "__mro__")) else {
        return None;
    };

    let http_exception = py.get_type::<PyHTTPException>();
    mro.try_iter().ok()?.find_map(|entry| {
        let entry = entry.ok()?;
        if let Ok(Some(handler)) = registry.get_item(&entry) {
            return Some(handler);
        }
        if entry.is(&http_exception) {
            let starlette = STARLETTE_EXCEPTIONS
                .get(py)
                .ok()?
                .getattr(intern!(py, "HTTPException"))
                .ok()?;
            return registry.get_item(starlette).ok().flatten();
        }
        None
    })
}

fastrapi_pyutil::cached_py_import!(STARLETTE_EXCEPTIONS, "starlette.exceptions");

fn build_request_arg(py: Python<'_>, request_input: Option<&RequestInput<'_>>) -> Py<PyAny> {
    request_input
        .and_then(|input| create_py_request(py, input, None).ok())
        .or_else(|| create_stub_request(py).ok())
        .unwrap_or_else(|| py.None())
}

pub(crate) fn dispatch_status_handler(server: EventLoop, status: u16) -> Option<Response> {
    let context = server.server();
    if !context.has_status_handlers() {
        return None;
    }
    let registry = context.exception_handlers()?;
    Python::attach(|py| status_handler_response(py, server, registry, status))
}

fn status_handler_response(
    py: Python<'_>,
    server: EventLoop,
    registry: &Py<PyDict>,
    status: u16,
) -> Option<Response> {
    let key = PyInt::new(py, status);
    let handler = registry.bind(py).get_item(key).ok().flatten()?;

    let exc = Py::new(
        py,
        PyHTTPException {
            status_code: status,
            detail: status_phrase(py, status),
            headers: None,
        },
    )
    .ok()?
    .into_any();
    let request = create_stub_request(py).ok()?;

    match handler.call1((request, exc.bind(py))) {
        Ok(result) => handler_response(py, server, result)
            .map(|result| result.into_response(StatusCode::INTERNAL_SERVER_ERROR.into_response())),
        Err(err) => {
            err.print(py);
            Some(StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}
