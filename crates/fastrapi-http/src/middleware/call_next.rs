//! This module contains the `call_next` support for starlette-style middleware.
//! It runs the next layers of the middleware stack and gives the result to Python as a `DownstreamResponse` object.

use fastrapi_runtime::py_bridge;
use fastrapi_runtime::py_bridge::EventLoop;
use std::sync::Arc;

use crate::exceptions::{UnhandledError, plain_server_error, server_error_response};
use crate::request::create_middleware_request;
use crate::request_input::RequestInput;
use crate::response_convert::convert_auto_response;
use crate::stream::SlotLease;
use axum::body::{Body, BodyDataStream, to_bytes};
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use bytes::Bytes;
use fastrapi_runtime::blocking::run_python;
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use parking_lot::Mutex;
use pyo3::exceptions::{PyRuntimeError, PyStopAsyncIteration};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict, PyList};
use pyo3::{Bound, intern};
use pyo3_async_runtimes::TaskLocals;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

#[pyclass(
    frozen,
    name = "DownstreamResponse",
    module = "fastrapi.middleware",
    skip_from_py_object
)]
pub struct PyDownstreamResponse {
    status_code: AtomicU16,
    #[pyo3(get)]
    pub raw_headers: Py<PyList>,
    stream: BodySlot,
}

type BodySlot = Arc<Mutex<Option<BodyDataStream>>>;

#[pymethods]
impl PyDownstreamResponse {
    #[getter]
    fn status_code(&self) -> u16 {
        self.status_code.load(Ordering::Relaxed)
    }

    #[setter]
    fn set_status_code(&self, status_code: u16) {
        self.status_code.store(status_code, Ordering::Relaxed);
    }

    #[getter]
    fn headers<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        crate::response_api::headers(py, &self.raw_headers)
    }

    #[getter]
    fn body_iterator(&self) -> PyBodyIterator {
        PyBodyIterator {
            stream: Arc::clone(&self.stream),
        }
    }

    #[pyo3(signature = (key, value=String::new(), max_age=None, expires=None, path=Some("/".to_owned()), domain=None, secure=false, httponly=false, samesite=Some("lax".to_owned()), partitioned=false))]
    #[expect(
        clippy::too_many_arguments,
        reason = "the signature copies starlette Response.set_cookie"
    )]
    fn set_cookie(
        &self,
        py: Python<'_>,
        key: String,
        value: String,
        max_age: Option<i64>,
        expires: Option<&Bound<'_, PyAny>>,
        path: Option<String>,
        domain: Option<String>,
        secure: bool,
        httponly: bool,
        samesite: Option<String>,
        partitioned: bool,
    ) -> PyResult<()> {
        crate::response_api::set_cookie(
            py,
            &self.raw_headers,
            key,
            value,
            max_age,
            expires,
            path,
            domain,
            secure,
            httponly,
            samesite,
            partitioned,
        )
    }

    #[pyo3(signature = (key, path=Some("/".to_owned()), domain=None, secure=false, httponly=false, samesite=Some("lax".to_owned()), partitioned=false))]
    #[expect(
        clippy::too_many_arguments,
        reason = "the signature copies starlette Response.delete_cookie"
    )]
    fn delete_cookie(
        &self,
        py: Python<'_>,
        key: String,
        path: Option<String>,
        domain: Option<String>,
        secure: bool,
        httponly: bool,
        samesite: Option<String>,
        partitioned: bool,
    ) -> PyResult<()> {
        let expires = pyo3::types::PyInt::new(py, 0);
        crate::response_api::set_cookie(
            py,
            &self.raw_headers,
            key,
            String::new(),
            Some(0),
            Some(expires.as_any()),
            path,
            domain,
            secure,
            httponly,
            samesite,
            partitioned,
        )
    }

    fn __repr__(&self) -> String {
        format!("DownstreamResponse(status_code={})", self.status_code())
    }
}

#[pyclass(frozen, name = "BodyIterator", module = "fastrapi.middleware")]
pub struct PyBodyIterator {
    stream: BodySlot,
}

#[pymethods]
impl PyBodyIterator {
    const fn __aiter__(slf: Bound<'_, Self>) -> Bound<'_, Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let slot = Arc::clone(&self.stream);
        py_bridge::future_into_py(py, async move {
            let Some(mut lease) = SlotLease::take(&slot) else {
                return Err(PyStopAsyncIteration::new_err(()));
            };
            let next = match lease.value() {
                Some(stream) => stream.next().await,
                None => None,
            };
            match next {
                Some(Ok(chunk)) => {
                    drop(lease);
                    Ok(Python::attach(|py| PyBytes::new(py, &chunk).unbind()))
                }
                Some(Err(err)) => {
                    lease.discard();
                    Err(PyRuntimeError::new_err(err.to_string()))
                }
                None => {
                    lease.discard();
                    Err(PyStopAsyncIteration::new_err(()))
                }
            }
        })
    }
}

#[pyclass(
    frozen,
    name = "CallNext",
    module = "fastrapi.middleware",
    skip_from_py_object
)]
pub struct PyCallNext {
    downstream: DownstreamSlot,
    event_loop: EventLoop,
}

type DownstreamSlot = Arc<Mutex<Option<Downstream>>>;

/// The `receive` of the request that a `call_next` middleware gets. It reads the request body
/// and puts it back, so that the next layers still get the body, like Starlette's cached request.
#[pyclass(
    frozen,
    name = "DownstreamReceive",
    module = "fastrapi.middleware",
    skip_from_py_object
)]
pub struct PyDownstreamReceive {
    downstream: DownstreamSlot,
    delivered: AtomicBool,
}

#[pymethods]
impl PyDownstreamReceive {
    fn __call__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let delivered = self.delivered.swap(true, Ordering::AcqRel);
        let body = self
            .downstream
            .lock()
            .as_mut()
            .filter(|_| !delivered)
            .map(|downstream| std::mem::take(downstream.request.body_mut()));
        let slot = Arc::clone(&self.downstream);
        py_bridge::future_into_py(py, async move {
            let body = match body {
                Some(body) => {
                    let body = to_bytes(body, usize::MAX)
                        .await
                        .map_err(|err| PyRuntimeError::new_err(err.to_string()))?;
                    if let Some(downstream) = slot.lock().as_mut() {
                        *downstream.request.body_mut() = Body::from(body.clone());
                    }
                    body
                }
                None => Bytes::new(),
            };
            Python::attach(|py| {
                let message = PyDict::new(py);
                if delivered {
                    message.set_item(intern!(py, "type"), intern!(py, "http.disconnect"))?;
                } else {
                    message.set_item(intern!(py, "type"), intern!(py, "http.request"))?;
                    message.set_item(intern!(py, "body"), PyBytes::new(py, &body))?;
                    message.set_item(intern!(py, "more_body"), false)?;
                }
                Ok(message.unbind())
            })
        })
    }
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
        let Some(mut downstream) = self.downstream.lock().take() else {
            return Err(PyRuntimeError::new_err("call_next was already called"));
        };
        downstream
            .request
            .extensions_mut()
            .insert(py_bridge::RequestContext::capture(py)?);
        let event_loop = self.event_loop;
        let bridged = async move {
            let Downstream {
                middlewares,
                idx,
                request,
                next,
            } = downstream;
            let mut response = drive(middlewares, idx + 1, request, next, event_loop).await;
            if let Some(UnhandledError(err)) = response.extensions_mut().remove() {
                return Err(Python::attach(|py| err.clone_ref(py)));
            }
            let (parts, body) = split_response(response);
            Python::attach(|py| Ok(parts_to_py(py, &parts, body)?.unbind()))
        };
        let locals = TaskLocals::new(event_loop.bind(py).clone());
        py_bridge::future_into_py_with_locals(py, locals, bridged)
    }
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
    body: Body,
) -> PyResult<Bound<'py, PyAny>> {
    let raw_headers = PyList::empty(py);
    for (name, value) in &parts.headers {
        raw_headers.append((
            PyBytes::new(py, name.as_str().as_bytes()),
            PyBytes::new(py, value.as_bytes()),
        ))?;
    }

    Bound::new(
        py,
        PyDownstreamResponse {
            status_code: AtomicU16::new(parts.status),
            raw_headers: raw_headers.unbind(),
            stream: Arc::new(Mutex::new(Some(body.into_data_stream()))),
        },
    )
    .map(Bound::into_any)
}

fn py_to_response(py: Python<'_>, value: &Bound<'_, PyAny>) -> Response {
    if let Ok(downstream) = value.cast::<PyDownstreamResponse>() {
        let ds = downstream.get();
        let status =
            StatusCode::from_u16(ds.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut builder = Response::builder().status(status);
        for item in ds.raw_headers.bind(py).iter() {
            if let Ok((name, value)) = item.extract::<(&[u8], &[u8])>() {
                builder = builder.header(name, value);
            }
        }
        let body = ds
            .stream
            .lock()
            .take()
            .map_or_else(Body::empty, Body::from_stream);
        return builder.body(body).unwrap_or_else(|_| plain_server_error());
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

fn drive(
    middlewares: Arc<Vec<Arc<super::PyMiddleware>>>,
    idx: usize,
    request: Request,
    next: Next,
    event_loop: EventLoop,
) -> BoxFuture<'static, Response> {
    let context = request
        .extensions()
        .get::<py_bridge::RequestContext>()
        .cloned();
    Box::pin(py_bridge::scoped_request_context(context, async move {
        let Some(mw) = middlewares.get(idx).cloned() else {
            return next.run(request).await;
        };
        let is_async = mw.is_async;

        let downstream = Downstream {
            middlewares,
            idx,
            request,
            next,
        };

        let call = move |py: Python<'_>| invoke(py, &mw, downstream, event_loop);
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
                        server_error_response(py, &err, Some(event_loop))
                    }
                })
            }
            _ => {
                tracing::error!("call_next middleware returned no response");
                plain_server_error()
            }
        }
    }))
}

fn invoke(
    py: Python<'_>,
    mw: &super::PyMiddleware,
    downstream: Downstream,
    event_loop: EventLoop,
) -> Result<Outcome, Response> {
    let fail = |err: PyErr| -> Response {
        err.print(py);
        server_error_response(py, &err, Some(event_loop))
    };

    let slot = Arc::new(Mutex::new(None));
    let receive = Bound::new(
        py,
        PyDownstreamReceive {
            downstream: Arc::clone(&slot),
            delivered: AtomicBool::new(false),
        },
    )
    .map_err(fail)?
    .into_any()
    .unbind();
    let req_obj = {
        let request = &downstream.request;
        let input = RequestInput::new(
            request.method(),
            request.uri(),
            request.headers(),
            request.extensions(),
            Some(event_loop),
        );
        create_middleware_request(py, &input, receive)
    }
    .map_err(fail)?
    .into_any()
    .unbind();
    *slot.lock() = Some(downstream);
    let downstream = slot;

    let call_next = Bound::new(
        py,
        PyCallNext {
            downstream,
            event_loop,
        },
    )
    .map_err(fail)?
    .into_any()
    .unbind();

    let result = mw.func.bind(py).call1((req_obj, call_next)).map_err(fail)?;

    if result.is_none() {
        tracing::error!("call_next middleware returned None without awaiting call_next");
        return Err(plain_server_error());
    }
    // A plain `def` middleware may return `call_next(request)` un-awaited, which Starlette still awaits.
    if mw.is_async || result.hasattr(intern!(py, "__await__")).map_err(fail)? {
        let fut = py_bridge::schedule_task(py, event_loop, result).map_err(fail)?;
        return Ok(Outcome::Await(fut));
    }

    Ok(Outcome::Respond(py_to_response(py, &result)))
}
