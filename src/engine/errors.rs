use axum::response::{IntoResponse, Response};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::http::{
    request::{create_py_request, create_stub_request},
    responses::convert_auto_response,
};
use crate::routing::types::RequestInput;

pub(crate) fn dispatch_exception_handler(
    py: Python<'_>,
    err: &PyErr,
    request_input: Option<&RequestInput<'_>>,
) -> Option<Response> {
    let registry = crate::globals::exception_handlers()?;
    let bound_registry = registry.bind(py);

    let handler = find_handler(py, bound_registry, err)?;
    let exc_value = err.value(py).clone();

    let request = build_request_arg(py, request_input);
    match handler.call1((request, exc_value)) {
        Ok(result) => Some(convert_auto_response(py, &result)),
        Err(handler_err) => {
            tracing::error!("Exception handler itself failed:");
            handler_err.print(py);
            Some(axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
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
    let registry = crate::globals::exception_handlers()?;
    let key = pyo3::types::PyInt::new(py, status);
    let handler = registry.bind(py).get_item(key).ok().flatten()?;

    let detail = "Not Found".to_string();
    let exc = pyo3::exceptions::PyException::new_err(detail);
    let request = create_stub_request(py).ok()?;

    match handler.call1((request, exc.value(py))) {
        Ok(result) => Some(convert_auto_response(py, &result)),
        Err(err) => {
            err.print(py);
            Some(axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}
