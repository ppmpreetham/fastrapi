//! The dispatch of errors to the exception handlers that the user registers.
//! The functions find the handler for an exception, a validation error or a status code.
//! They call the handler and convert its result into an HTTP response.

use axum::http::header::ALLOW;
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3::types::PyType;
use pyo3::{Py, intern};

use ahash::AHashMap;
use fastrapi_http::exceptions::{
    http_status_code, is_default_handler, is_http_exception, plain_server_error,
    request_validation_error_type, websocket_validation_error_type,
};
use fastrapi_http::request::{create_py_request, create_stub_request};
use fastrapi_http::request_input::RequestInput;
use fastrapi_http::response_convert::convert_auto_response;
use fastrapi_runtime::py_bridge::{EventLoop, PyTaskFuture, schedule_task};
use fastrapi_runtime::server_state::current_server;
use parking_lot::Mutex;
use std::sync::{Arc, LazyLock};

type HandlerSlot = (Py<PyAny>, usize, Option<Py<PyAny>>);

static HANDLER_CACHE: LazyLock<Mutex<AHashMap<(usize, usize), HandlerSlot>>> =
    LazyLock::new(|| Mutex::new(AHashMap::new()));

/// Forgets every cached handler lookup. It runs when a handler is registered
/// or replaced and when a server installs its registry, never per request, so
/// a replaced handler or a reused registry address can't serve a stale entry.
pub(crate) fn invalidate_handler_cache() {
    HANDLER_CACHE.lock().clear();
}

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

pub(crate) fn dispatch_exception_handler(
    py: Python<'_>,
    err: &PyErr,
    request_input: Option<&RequestInput<'_>>,
    server: Option<EventLoop>,
) -> Option<Response> {
    let server = server.or_else(|| server_of(request_input))?;
    let registry = server.server().exception_handlers()?.bind(py);
    let exc_value = err.value(py);
    let http_error = is_http_exception(exc_value);
    let handler = http_error
        .then(|| http_status_code(exc_value))
        .flatten()
        .and_then(|status| registry.get_item(status).ok().flatten())
        .or_else(|| find_handler(py, registry, err))
        .or_else(|| {
            (!http_error)
                .then(|| registry.get_item(500).ok().flatten())
                .flatten()
        })?;
    let request = build_request_arg(py, request_input);
    match handler.call1((request, exc_value)) {
        Ok(result) => handler_response(py, server, result)
            .map(|result| result.into_response(plain_server_error())),
        Err(handler_err) => {
            tracing::error!("Exception handler itself failed:");
            handler_err.print(py);
            Some(plain_server_error())
        }
    }
}

pub(crate) fn has_custom_handlers(py: Python<'_>, server: EventLoop) -> bool {
    server
        .server()
        .exception_handlers()
        .is_some_and(|registry| {
            registry
                .bind(py)
                .values()
                .iter()
                .any(|handler| !is_default_handler(&handler))
        })
}

pub(crate) fn dispatch_http_error(
    server: EventLoop,
    status: u16,
    request_input: &RequestInput<'_>,
) -> Option<Response> {
    Python::attach(|py| {
        let exc = STARLETTE_HTTP_EXCEPTION
            .get(py)
            .ok()?
            .call1((status,))
            .ok()?;
        dispatch_exception_handler(
            py,
            &PyErr::from_value(exc),
            Some(request_input),
            Some(server),
        )
    })
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
    ty: &Bound<'py, PyType>,
) -> Option<Bound<'py, PyAny>> {
    let Ok(mro) = ty.getattr(intern!(py, "__mro__")) else {
        return None;
    };
    let validation_error = request_validation_error_type(py).ok()?;
    let websocket_validation_error = websocket_validation_error_type(py).ok()?;
    mro.try_iter().ok()?.find_map(|entry| {
        let entry = entry.ok()?;
        if let Ok(Some(handler)) = registry.get_item(&entry) {
            return Some((!is_default_handler(&handler)).then_some(handler));
        }
        let has_default = STARLETTE_HTTP_EXCEPTION.is(py, &entry)
            || entry.is(validation_error)
            || entry.is(websocket_validation_error);
        has_default.then_some(None)
    })?
}

fastrapi_pyutil::cached_py_import!(
    STARLETTE_HTTP_EXCEPTION,
    "starlette.exceptions",
    "HTTPException"
);

fn build_request_arg(py: Python<'_>, request_input: Option<&RequestInput<'_>>) -> Py<PyAny> {
    request_input
        .and_then(|input| create_py_request(py, input, None).ok())
        .or_else(|| create_stub_request(py).ok())
        .unwrap_or_else(|| py.None())
}

/// The exception handlers that answer the router's own 404 and 405, resolved once
/// from the app's registry the way Starlette's `ExceptionMiddleware` picks one for
/// the `HTTPException` its router raises: the handler for the status code, else
/// the first one on the exception's MRO. `None` keeps the default response.
pub(crate) struct RoutingErrorHandlers {
    not_found: Option<Py<PyAny>>,
    method_not_allowed: Option<Py<PyAny>>,
}

impl RoutingErrorHandlers {
    pub(crate) fn resolve(py: Python<'_>, registry: &Bound<'_, PyDict>) -> PyResult<Self> {
        let exception = STARLETTE_HTTP_EXCEPTION.get(py)?.cast::<PyType>()?;
        let handler = |status: StatusCode| -> PyResult<Option<Py<PyAny>>> {
            let handler = match registry.get_item(status.as_u16())? {
                Some(handler) => (!is_default_handler(&handler)).then_some(handler),
                None => lookup_handler(py, registry, exception),
            };
            Ok(handler.map(Bound::unbind))
        };
        Ok(Self {
            not_found: handler(StatusCode::NOT_FOUND)?,
            method_not_allowed: handler(StatusCode::METHOD_NOT_ALLOWED)?,
        })
    }

    pub(crate) fn for_status(&self, status: StatusCode) -> Option<&Py<PyAny>> {
        match status {
            StatusCode::NOT_FOUND => self.not_found.as_ref(),
            StatusCode::METHOD_NOT_ALLOWED => self.method_not_allowed.as_ref(),
            _ => None,
        }
    }
}

/// Calls `handler` with the `HTTPException` that Starlette's router raises for `status`,
/// carrying the `Allow` header of a 405.
pub(crate) fn dispatch_status_handler(
    server: EventLoop,
    handler: &Py<PyAny>,
    status: StatusCode,
    allow: Option<&HeaderValue>,
    request_input: &RequestInput<'_>,
) -> Option<Response> {
    Python::attach(|py| {
        let headers = allow
            .map(|allow| -> PyResult<_> {
                let headers = PyDict::new(py);
                let allow = allow
                    .to_str()
                    .map_err(|err| PyValueError::new_err(err.to_string()))?;
                headers.set_item(ALLOW.as_str(), allow)?;
                Ok(headers)
            })
            .transpose();
        let exc = headers.and_then(|headers| {
            STARLETTE_HTTP_EXCEPTION
                .get(py)?
                .call1((status.as_u16(), py.None(), headers))
        });
        let result = exc.and_then(|exc| {
            handler
                .bind(py)
                .call1((build_request_arg(py, Some(request_input)), exc))
        });
        match result {
            Ok(result) => handler_response(py, server, result)
                .map(|result| result.into_response(plain_server_error())),
            Err(err) => {
                err.print(py);
                Some(plain_server_error())
            }
        }
    })
}
