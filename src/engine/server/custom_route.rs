use super::dispatch::dispatch_or_not_found;
use super::serve::AppState;
use crate::engine::executor::build_request_input_from_parts;
use crate::engine::handler_errors::python_error_to_response_in;
use crate::ffi::routing::PyAPIRoute;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use fastrapi_core::params::PathParamRange;
use fastrapi_core::router::FrozenRouter;
use fastrapi_http::request::create_py_request;
use fastrapi_http::request_input::RaisedError;
use fastrapi_http::response_convert::convert_auto_response;
use fastrapi_http::responses::PyResponse;
use fastrapi_routing::route::{RouteEntry, RouteHandler};
use fastrapi_runtime::py_bridge;
use pyo3::exceptions::PyValueError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use smallvec::SmallVec;

#[derive(Clone, Copy)]
pub(crate) struct BypassCustomHandler;

#[pyclass(frozen, name = "RouteHandler", module = "fastrapi.routing")]
pub(crate) struct PyOriginalHandler {
    router: &'static FrozenRouter<RouteHandler>,
    state: &'static AppState,
}

fn invalid(err: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(err.to_string())
}

fn axum_request(request: &Bound<'_, PyAny>, body: Bytes, raised: RaisedError) -> PyResult<Request> {
    let py = request.py();
    let scope = request.getattr(intern!(py, "scope"))?;
    let method: String = scope.get_item(intern!(py, "method"))?.extract()?;
    let path: Vec<u8> = scope.get_item(intern!(py, "raw_path"))?.extract()?;
    let query: Vec<u8> = scope
        .get_item(intern!(py, "query_string"))?
        .extract()
        .unwrap_or_default();
    let mut target = path;
    if !query.is_empty() {
        target.push(b'?');
        target.extend_from_slice(&query);
    }
    let mut builder = Request::builder()
        .method(method.as_str())
        .uri(target)
        .extension(BypassCustomHandler)
        .extension(raised);
    for pair in scope.get_item(intern!(py, "headers"))?.try_iter()? {
        let (name, value): (Vec<u8>, Vec<u8>) = pair?.extract()?;
        builder = builder.header(
            HeaderName::from_bytes(&name).map_err(invalid)?,
            HeaderValue::from_bytes(&value).map_err(invalid)?,
        );
    }
    builder.body(Body::from(body)).map_err(invalid)
}

async fn python_response(response: Response) -> PyResult<Py<PyAny>> {
    let (parts, body) = response.into_parts();
    let body = to_bytes(body, usize::MAX).await.map_err(invalid)?;
    Python::attach(|py| {
        let kwargs = PyDict::new(py);
        kwargs.set_item(intern!(py, "status_code"), parts.status.as_u16())?;
        let response = py
            .get_type::<PyResponse>()
            .call((PyBytes::new(py, &body),), Some(&kwargs))?;
        let headers = PyList::empty(py);
        for (name, value) in &parts.headers {
            headers.append((
                PyBytes::new(py, name.as_ref()),
                PyBytes::new(py, value.as_bytes()),
            ))?;
        }
        response.setattr(intern!(py, "raw_headers"), headers)?;
        Ok(response.unbind())
    })
}

#[pymethods]
impl PyOriginalHandler {
    fn __call__<'py>(&self, request: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = request.py();
        let locals = py_bridge::get_current_locals(py)?;
        let body = py_bridge::into_future_with_locals(
            &locals,
            request.call_method0(intern!(py, "body"))?,
        )?;
        let request = request.clone().unbind();
        let (router, state) = (self.router, self.state);
        py_bridge::future_into_py_with_locals(py, locals, async move {
            let body = body.await?;
            let raised = RaisedError::default();
            let request = Python::attach(|py| {
                let body = Bytes::from(body.bind(py).extract::<Vec<u8>>()?);
                axum_request(request.bind(py), body, raised.clone())
            })?;
            let response = match dispatch_or_not_found(router, state, request).await {
                Ok(response) => response,
                Err(_) => StatusCode::NOT_FOUND.into_response(),
            };
            if let Some(err) = raised.take() {
                return Err(err);
            }
            python_response(response).await
        })
    }
}

pub(crate) fn install_custom_handlers(
    py: Python<'_>,
    routes: &[RouteEntry],
    router: &'static FrozenRouter<RouteHandler>,
    state: &'static AppState,
) -> PyResult<()> {
    let base = py
        .get_type::<PyAPIRoute>()
        .getattr(intern!(py, "get_route_handler"))?;
    for route in routes {
        let Some(instance) = &route.route_instance else {
            continue;
        };
        let instance = instance.bind(py);
        if instance
            .get_type()
            .getattr(intern!(py, "get_route_handler"))?
            .is(&base)
        {
            continue;
        }
        instance.setattr(
            intern!(py, "_fastrapi_original"),
            PyOriginalHandler { router, state },
        )?;
        let custom = instance.call_method0(intern!(py, "get_route_handler"))?;
        _ = route.handler.execution.custom_handler.set(custom.unbind());
    }
    Ok(())
}

pub(crate) async fn run_custom_handler(
    custom: &Py<PyAny>,
    handler: &'static RouteHandler,
    state: &'static AppState,
    request: Request,
    param_ranges: SmallVec<[PathParamRange; 4]>,
) -> Response {
    let (parts, body) = request.into_parts();
    let Ok(body) = to_bytes(body, state.max_body_size.unwrap_or(usize::MAX)).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let event_loop = state.event_loop;
    let input = build_request_input_from_parts(
        &parts,
        &param_ranges,
        &handler.payload.path_param_names,
        event_loop,
    );
    let scheduled = Python::attach(|py| {
        let request = create_py_request(py, &input, Some(body))?;
        let coroutine = custom.bind(py).call1((request,))?;
        py_bridge::schedule_task(py, event_loop, coroutine)
    });
    let result = match scheduled {
        Ok(pending) => pending.await,
        Err(err) => Err(err),
    };
    Python::attach(|py| match result {
        Ok(value) => convert_auto_response(py, value.bind(py)),
        Err(err) => python_error_to_response_in(py, &err, Some(&input)),
    })
}
