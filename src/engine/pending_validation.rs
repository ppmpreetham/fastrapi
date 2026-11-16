//! FastAPI runs a route's dependencies before it reports a request validation error.
//! An error from a dependency wins over the 422, and the dependency's own validation
//! errors are listed before the endpoint's.

use fastrapi_http::exceptions::{request_validation_error, request_validation_error_type};
use fastrapi_routing::request_data::Rejection;
use fastrapi_routing::route::RouteHandler;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyList;

/// The endpoint's validation error, held while the dependencies run.
pub(crate) struct PendingValidation(PyErr);

impl PendingValidation {
    /// Holds the rejection when it is a validation error on a route with dependencies, or on a
    /// route whose telemetry reports the failure once the dependency span ends.
    pub(crate) fn hold(
        py: Python<'_>,
        handler: &RouteHandler,
        rejection: Rejection,
    ) -> Result<Self, Rejection> {
        match rejection {
            Rejection::Python(err)
                if (!handler.payload.dependencies.is_empty() || handler.execution.operation_spans)
                    && is_request_validation_error(py, &err) =>
            {
                Ok(Self(err))
            }
            other => Err(other),
        }
    }

    pub(crate) const fn error(&self) -> &PyErr {
        &self.0
    }

    pub(crate) fn into_error(self) -> PyErr {
        self.0
    }

    /// Combines a dependency failure with the held error, in FastAPI's order.
    pub(crate) fn after_dependency_error(self, py: Python<'_>, dependency: PyErr) -> PyErr {
        if !is_request_validation_error(py, &dependency) {
            return dependency;
        }
        combined_errors(py, &dependency, &self.0).unwrap_or(dependency)
    }
}

/// Combines a dependency failure with an optional held error.
pub(crate) fn dependency_failure(
    py: Python<'_>,
    pending: Option<PendingValidation>,
    dependency: PyErr,
) -> PyErr {
    match pending {
        Some(pending) => pending.after_dependency_error(py, dependency),
        None => dependency,
    }
}

fn is_request_validation_error(py: Python<'_>, err: &PyErr) -> bool {
    request_validation_error_type(py).is_ok_and(|class| err.is_instance(py, class))
}

fn combined_errors(py: Python<'_>, dependency: &PyErr, endpoint: &PyErr) -> PyResult<PyErr> {
    let errors = PyList::empty(py);
    for err in [dependency, endpoint] {
        for item in err.value(py).call_method0(intern!(py, "errors"))?.try_iter()? {
            errors.append(item?)?;
        }
    }
    let body = endpoint.value(py).getattr(intern!(py, "body"))?;
    Ok(request_validation_error(py, errors.into_any(), Some(body)))
}
