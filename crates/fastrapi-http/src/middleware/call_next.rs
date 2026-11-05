//! This module contains the `call_next` support for starlette-style middleware.
//! It runs the next layers of the middleware stack and gives the result to Python as a `DownstreamResponse` object.

use fastrapi_runtime::py_bridge;
use fastrapi_runtime::py_bridge::EventLoop;
use std::sync::Arc;

use crate::request::PyRequest;
use crate::response_convert::convert_auto_response;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use fastrapi_runtime::blocking::run_python;
use futures_util::future::BoxFuture;
use parking_lot::Mutex;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict};
use pyo3::{Bound, intern};
use pyo3_async_runtimes::TaskLocals;

#[pyclass(
    frozen,
    name = "DownstreamResponse",
    module = "fastrapi.middleware",
    skip_from_py_object
)]
pub struct PyDownstreamResponse {
    #[pyo3(get)]
    pub status_code: u16,
    #[pyo3(get)]
    pub headers: Py<PyDict>,
    pub body_bytes: bytes::Bytes,
}

#[pymethods]
impl PyDownstreamResponse {
    fn __repr__(&self) -> String {
        format!("DownstreamResponse(status_code={})", self.status_code)
    }

    #[getter]
    fn body<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.body_bytes)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body_bytes).into_owned()
    }
}

#[pyclass(
    frozen,
    name = "CallNext",
    module = "fastrapi.middleware",
    skip_from_py_object
)]
pub struct PyCallNext {
    downstream: Mutex<Option<Downstream>>,
    event_loop: EventLoop,
}

struct Downstream {
    middlewares: Arc<Vec<Arc<super::PyMiddleware>>>,
    idx: usize,
    request: Request,
    next: Next,
}

#[pymethods]
impl PyCallNext {
    fn __call__<'py>(
        &self,
        py: Python<'py>,
        _request: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let Some(downstream) = self.downstream.lock().take() else {
            return Err(PyRuntimeError::new_err("call_next was already called"));
        };
        let event_loop = self.event_loop;
        let bridged = async move {
            let Downstream {
                middlewares,
                idx,
                request,
                next,
            } = downstream;
            let response = drive(middlewares, idx + 1, request, next, event_loop).await;
            let (parts, body) = split_response(response);
            let body_bytes = to_bytes(body, usize::MAX)
                .await
                .map_err(|err| PyRuntimeError::new_err(err.to_string()))?;
            Python::attach(|py| Ok(parts_to_py(py, &parts, body_bytes)?.unbind()))
        };
        let locals = TaskLocals::new(event_loop.bind(py).clone());
        py_bridge::future_into_py_with_locals(py, locals, bridged)
    }
}

fn middleware_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Middleware Execution Error",
    )
        .into_response()
}

struct DownstreamParts {
    status: u16,
    headers: HeaderMap,
}

fn split_response(response: Response) -> (DownstreamParts, Body) {
    let (parts, body) = response.into_parts();
    (
        DownstreamParts {
            status: parts.status.as_u16(),
            headers: parts.headers,
        },
        body,
    )
}

fn parts_to_py<'py>(
    py: Python<'py>,
    parts: &DownstreamParts,
    body: bytes::Bytes,
) -> PyResult<Bound<'py, PyAny>> {
    let headers_dict = PyDict::new(py);
    for (name, value) in &parts.headers {
        if let Ok(value) = value.to_str() {
            headers_dict.set_item(name.as_str(), value)?;
        }
    }

    Bound::new(
        py,
        PyDownstreamResponse {
            status_code: parts.status,
            headers: headers_dict.unbind(),
            body_bytes: body,
        },
    )
    .map(Bound::into_any)
}

fn py_to_response(py: Python<'_>, value: &Bound<'_, PyAny>) -> Response {
    if let Ok(downstream) = value.cast::<PyDownstreamResponse>() {
        let ds = downstream.borrow();
        let status =
            StatusCode::from_u16(ds.status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut builder = Response::builder().status(status);
        if let Ok(dict) = ds.headers.bind(py).cast::<PyDict>() {
            for (k, v) in dict.iter() {
                if let (Ok(k), Ok(v)) = (k.extract::<&str>(), v.extract::<&str>()) {
                    builder = builder.header(k, v);
                }
            }
        }
        return builder
            .body(Body::from(ds.body_bytes.clone()))
            .unwrap_or_else(|_| middleware_error());
    }

    convert_auto_response(py, value)
}

enum Outcome {
    Respond(Response),
    Await(py_bridge::PyTaskFuture),
}

pub(crate) async fn run_call_next_stack(
    middlewares: &Arc<Vec<Arc<super::PyMiddleware>>>,
    request: Request,
    next: Next,
    event_loop: EventLoop,
) -> Response {
    drive(middlewares.clone(), 0, request, next, event_loop).await
}

#[allow(clippy::too_many_arguments)]
fn drive(
    middlewares: Arc<Vec<Arc<super::PyMiddleware>>>,
    idx: usize,
    request: Request,
    next: Next,
    event_loop: EventLoop,
) -> BoxFuture<'static, Response> {
    Box::pin(async move {
        let Some(mw) = middlewares.get(idx).cloned() else {
            return next.run(request).await;
        };
        let is_async = mw.is_async;

        let (parts, body) = request.into_parts();
        let method = parts.method.clone();
        let uri = parts.uri.clone();
        let headers = Arc::new(parts.headers.clone());
        let downstream = Downstream {
            middlewares,
            idx,
            request: Request::from_parts(parts, body),
            next,
        };

        let call = move |py: Python<'_>| -> Result<Outcome, Response> {
            invoke(
                py,
                &mw,
                method.as_str(),
                uri.path(),
                uri.query().unwrap_or(""),
                headers,
                downstream,
                event_loop,
            )
        };
        let outcome = if is_async {
            Ok(Python::attach(call))
        } else {
            run_python(call).await
        };

        match outcome {
            Ok(Ok(Outcome::Respond(response))) => response,
            Ok(Ok(Outcome::Await(fut))) => {
                let awaited = fut.await;
                Python::attach(|py| match awaited {
                    Ok(value) => py_to_response(py, value.bind(py)),
                    Err(err) => {
                        err.print(py);
                        middleware_error()
                    }
                })
            }
            _ => {
                tracing::error!("call_next middleware returned no response");
                middleware_error()
            }
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn invoke(
    py: Python<'_>,
    mw: &super::PyMiddleware,
    method: &str,
    path: &str,
    query: &str,
    headers: Arc<HeaderMap>,
    downstream: Downstream,
    event_loop: EventLoop,
) -> Result<Outcome, Response> {
    let fail = |err: PyErr| -> Response {
        err.print(py);
        middleware_error()
    };

    let scope = PyDict::new(py);
    _ = scope.set_item(intern!(py, "type"), intern!(py, "http"));
    _ = scope.set_item(intern!(py, "method"), method);
    _ = scope.set_item(intern!(py, "path"), path);
    _ = scope.set_item(
        intern!(py, "query_string"),
        PyBytes::new(py, query.as_bytes()),
    );

    let req_obj = PyRequest::create_bound(py, scope.into_any().unbind(), Some(headers), None)
        .map_err(fail)?
        .into_any()
        .unbind();

    let call_next = Bound::new(
        py,
        PyCallNext {
            downstream: Mutex::new(Some(downstream)),
            event_loop,
        },
    )
    .map_err(fail)?
    .into_any()
    .unbind();

    let result = mw.func.bind(py).call1((req_obj, call_next)).map_err(fail)?;

    if result.is_none() {
        tracing::error!("call_next middleware returned None without awaiting call_next");
        return Err(middleware_error());
    }
    if mw.is_async {
        let fut = py_bridge::schedule_task(py, event_loop, result).map_err(fail)?;
        return Ok(Outcome::Await(fut));
    }

    Ok(Outcome::Respond(py_to_response(py, &result)))
}
