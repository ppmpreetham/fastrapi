//! This module runs raw ASGI middleware classes.
//! It gives `receive` and `send` callables to the middleware and connects them to the axum stack.
//! It collects the response messages that the middleware sends.

use crate::exceptions::{http_exception_response, plain_server_error, server_error_response};
use axum::body::{Body, to_bytes};
use axum::extract::{OriginalUri, Request};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use bytes::{Bytes, BytesMut};
use fastrapi_runtime::py_bridge;
use fastrapi_runtime::py_bridge::EventLoop;
use fastrapi_runtime::tokio_runtime::spawn;
use futures_util::StreamExt;
use futures_util::stream::unfold;
use parking_lot::Mutex;
use pyo3::exceptions::PyRuntimeError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyByteArray, PyBytes, PyDict, PyList};
use std::pin::pin;
use std::sync::Arc;
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::sync::oneshot;
use tokio::sync::watch;
use tracing::error;

const DOWNSTREAM_KEY: &str = "_fastrapi_downstream";

#[pyclass(
    frozen,
    name = "DownstreamASGI",
    module = "fastrapi.middleware",
    skip_from_py_object
)]
pub struct PyDownstreamASGI {
    next: Next,
    head: Parts,
    locals: py_bridge::TaskLocals,
}

#[pyclass(
    frozen,
    name = "ScopeDownstream",
    module = "fastrapi.middleware",
    skip_from_py_object
)]
pub struct PyScopeDownstream;

#[pyclass(
    frozen,
    name = "Receive",
    module = "fastrapi.middleware",
    skip_from_py_object
)]
pub struct PyAsgiReceive {
    body: Arc<Mutex<Option<bytes::Bytes>>>,
    finished: watch::Receiver<bool>,
}

#[pyclass(
    frozen,
    name = "Send",
    module = "fastrapi.middleware",
    skip_from_py_object
)]
pub struct PyAsgiSend {
    tx: Sender<Py<PyAny>>,
    finished: watch::Sender<bool>,
}

#[pymethods]
impl PyScopeDownstream {
    fn __call__<'py>(
        &self,
        py: Python<'py>,
        scope: &Bound<'py, PyDict>,
        receive: &Bound<'py, PyAny>,
        send: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let handle = scope
            .get_item(intern!(py, DOWNSTREAM_KEY))?
            .and_then(|item| item.cast::<PyDownstreamASGI>().ok().cloned());

        let Some(handle) = handle else {
            return py_bridge::future_into_py(py, async move {
                Err::<Py<PyAny>, _>(PyRuntimeError::new_err(
                    "asgi middleware called the downstream app with a scope that has no request attached",
                ))
            });
        };

        let borrowed = handle.borrow();
        let mut head = borrowed.head.clone();
        head.extensions
            .insert(py_bridge::RequestContext::capture(py)?);
        let downstream = PyDownstreamASGI {
            next: borrowed.next.clone(),
            head,
            locals: borrowed.locals.clone(),
        };
        let receive = receive.clone().unbind();
        let send = send.clone().unbind();

        py_bridge::future_into_py(py, async move {
            run_downstream(downstream, receive, send).await
        })
    }
}

/// Runs the remaining part of the axum stack.
/// It reads the body from the `receive` callable of the middleware.
/// It sends the response through the `send` callable of the middleware.
async fn run_downstream(
    downstream: PyDownstreamASGI,
    receive: Py<PyAny>,
    send: Py<PyAny>,
) -> PyResult<()> {
    let PyDownstreamASGI { next, head, locals } = downstream;

    let body = drain_receive(&locals, &receive).await?;
    let req = Request::from_parts(head, Body::from(body));
    let resp = next.run(req).await;
    send_response(&locals, &send, resp).await
}

/// Calls the `receive` callable of the middleware and assembles the body again.
pub async fn drain_receive(
    locals: &py_bridge::TaskLocals,
    receive: &Py<PyAny>,
) -> PyResult<bytes::Bytes> {
    let mut buf = BytesMut::new();
    loop {
        let msg: Py<PyAny> = {
            let fut = Python::attach(|py| -> PyResult<_> {
                let awaitable = receive.bind(py).call0()?;
                py_bridge::into_future_with_locals(locals, awaitable)
            })?;
            fut.await?
        };

        let (chunk, more) = Python::attach(|py| parse_request_message(py, msg.bind(py)));
        let Some(chunk) = chunk else {
            break;
        };
        buf.extend(chunk);
        if !more {
            break;
        }
    }
    Ok(buf.freeze())
}

/// Reads (`body`, `more_body`) from an ASGI `http.request` message.
/// The body is `None` when the client disconnects.
fn parse_request_message(py: Python<'_>, msg: &Bound<'_, PyAny>) -> (Option<bytes::Bytes>, bool) {
    let Ok(kind) = msg.get_item(intern!(py, "type")) else {
        return (None, false);
    };
    if kind
        .extract::<String>()
        .is_ok_and(|kind| kind == "http.disconnect")
    {
        return (None, false);
    }

    let body = msg
        .get_item(intern!(py, "body"))
        .ok()
        .and_then(|raw| python_bytes(py, &raw));
    let more = msg
        .get_item(intern!(py, "more_body"))
        .ok()
        .and_then(|value| value.is_truthy().ok())
        .unwrap_or(false);
    (body, more)
}

fn python_bytes(py: Python<'_>, obj: &Bound<'_, PyAny>) -> Option<bytes::Bytes> {
    if let Ok(bytes) = obj.cast::<PyBytes>() {
        return Some(fastrapi_pyutil::body::bytes_body(bytes));
    }
    if let Ok(bytearray) = obj.cast::<PyByteArray>() {
        return Some(Bytes::from(bytearray.to_vec()));
    }
    let as_bytes = obj.call_method0(intern!(py, "__bytes__")).ok()?;
    let bytes = as_bytes.cast::<PyBytes>().ok()?;
    Some(fastrapi_pyutil::body::bytes_body(bytes))
}

/// Sends `http.response.start`, then one `http.response.body` message per body frame.
pub async fn send_response(
    locals: &py_bridge::TaskLocals,
    send: &Py<PyAny>,
    resp: Response,
) -> PyResult<()> {
    let (parts, body) = resp.into_parts();
    let start = Python::attach(|py| -> PyResult<Py<PyAny>> {
        let msg = PyDict::new(py);
        msg.set_item(intern!(py, "type"), intern!(py, "http.response.start"))?;
        msg.set_item(intern!(py, "status"), parts.status.as_u16())?;
        let headers = PyList::empty(py);
        for (name, value) in &parts.headers {
            headers.append((
                PyBytes::new(py, name.as_str().as_bytes()),
                PyBytes::new(py, value.as_bytes()),
            ))?;
        }
        msg.set_item(intern!(py, "headers"), headers)?;
        Ok(msg.unbind().into_any())
    })?;
    asgi_call(locals, send, start).await?;

    let mut frames = body.into_data_stream();
    while let Some(chunk) = frames.next().await {
        let chunk = chunk.map_err(|err| PyRuntimeError::new_err(err.to_string()))?;
        if !chunk.is_empty() {
            asgi_call(locals, send, body_message(&chunk, true)?).await?;
        }
    }
    asgi_call(locals, send, body_message(&[], false)?).await
}

fn body_message(chunk: &[u8], more_body: bool) -> PyResult<Py<PyAny>> {
    Python::attach(|py| {
        let msg = PyDict::new(py);
        msg.set_item(intern!(py, "type"), intern!(py, "http.response.body"))?;
        msg.set_item(intern!(py, "body"), PyBytes::new(py, chunk))?;
        msg.set_item(intern!(py, "more_body"), more_body)?;
        Ok(msg.unbind().into_any())
    })
}

/// Awaits one `send(message)` call on the Python event loop.
pub async fn asgi_call(
    locals: &py_bridge::TaskLocals,
    target: &Py<PyAny>,
    message: Py<PyAny>,
) -> PyResult<()> {
    let fut = Python::attach(|py| -> PyResult<_> {
        let awaitable = target.bind(py).call1((message.bind(py),))?;
        py_bridge::into_future_with_locals(locals, awaitable)
    })?;
    fut.await?;
    Ok(())
}

#[pymethods]
impl PyAsgiReceive {
    fn __call__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let body = self.body.clone();
        let mut finished = self.finished.clone();
        py_bridge::future_into_py(py, async move {
            let body = body.lock().take();
            if body.is_none() {
                _ = finished.wait_for(|finished| *finished).await;
            }
            Python::attach(|py| {
                let msg = PyDict::new(py);
                match body {
                    Some(body) => {
                        msg.set_item(intern!(py, "type"), intern!(py, "http.request"))?;
                        msg.set_item(intern!(py, "body"), PyBytes::new(py, &body))?;
                        msg.set_item(intern!(py, "more_body"), false)?;
                    }
                    None => {
                        msg.set_item(intern!(py, "type"), intern!(py, "http.disconnect"))?;
                    }
                }
                Ok(msg.unbind().into_any())
            })
        })
    }
}

#[pymethods]
impl PyAsgiSend {
    fn __call__<'py>(
        &self,
        py: Python<'py>,
        message: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let tx = self.tx.clone();
        let completes = matches!(
            parse_response_message(py, message),
            ResponseMessage::Body(_, false)
        );
        let finished = self.finished.clone();
        let message = message.clone().unbind();
        py_bridge::future_into_py(py, async move {
            tx.send(message)
                .await
                .map_err(|_| PyRuntimeError::new_err("asgi send channel closed"))?;
            if completes {
                finished.send_replace(true);
            }
            Ok(())
        })
    }
}

/// A parsed `http.response.*` message that the middleware sent.
enum ResponseMessage {
    Start(u16, Vec<(bytes::Bytes, bytes::Bytes)>),
    Body(bytes::Bytes, bool),
    Other,
}

pub async fn run_asgi_request(
    instance: Arc<Py<PyAny>>,
    req: Request,
    next: Next,
    event_loop: EventLoop,
    max_body_size: usize,
) -> Response {
    let context = req.extensions().get::<py_bridge::RequestContext>().cloned();
    py_bridge::scoped_request_context(
        context,
        run_asgi_app(instance, req, next, event_loop, max_body_size),
    )
    .await
}

async fn run_asgi_app(
    instance: Arc<Py<PyAny>>,
    req: Request,
    next: Next,
    event_loop: EventLoop,
    max_body_size: usize,
) -> Response {
    let (parts, body) = req.into_parts();
    let Ok(body_bytes) = to_bytes(body, max_body_size).await else {
        return (StatusCode::PAYLOAD_TOO_LARGE, "Request body too large").into_response();
    };
    let scope = Python::attach(|py| -> PyResult<_> {
        let locals = py_bridge::TaskLocals::new(event_loop.bind(py).clone());
        let scope = build_scope(py, &parts, event_loop)?;
        let handle = Py::new(
            py,
            PyDownstreamASGI {
                next,
                head: parts,
                locals,
            },
        )?;
        scope
            .bind(py)
            .set_item(intern!(py, DOWNSTREAM_KEY), handle)?;
        Ok(scope)
    });
    drive_asgi(&instance, scope, body_bytes, event_loop)
        .await
        .unwrap_or_else(|err| server_error(&err, event_loop))
}

pub async fn mounted_asgi(
    app: Arc<Py<PyAny>>,
    prefix: Arc<str>,
    req: Request,
    event_loop: EventLoop,
    max_body_size: usize,
) -> Response {
    let (mut parts, body) = req.into_parts();
    if let Some(OriginalUri(uri)) = parts.extensions.get::<OriginalUri>().cloned() {
        parts.uri = uri;
    }
    let Ok(body_bytes) = to_bytes(body, max_body_size).await else {
        return (StatusCode::PAYLOAD_TOO_LARGE, "Request body too large").into_response();
    };
    let scope = Python::attach(|py| -> PyResult<_> {
        let scope = build_scope(py, &parts, event_loop)?;
        scope
            .bind(py)
            .set_item(intern!(py, "root_path"), &*prefix)?;
        Ok(scope)
    });
    drive_asgi(&app, scope, body_bytes, event_loop)
        .await
        .unwrap_or_else(|err| {
            Python::attach(|py| http_exception_response(err.value(py)))
                .unwrap_or_else(|| server_error(&err, event_loop))
        })
}

fn server_error(err: &PyErr, event_loop: EventLoop) -> Response {
    Python::attach(|py| {
        err.print(py);
        server_error_response(py, err, Some(event_loop))
    })
}

async fn drive_asgi(
    instance: &Py<PyAny>,
    scope: PyResult<Py<PyDict>>,
    body_bytes: Bytes,
    event_loop: EventLoop,
) -> PyResult<Response> {
    let (send_tx, send_rx) = channel::<Py<PyAny>>(16);
    let (finished_tx, finished_rx) = watch::channel(false);

    let outcome = Python::attach(|py| -> PyResult<_> {
        let scope = scope?;
        let receive = Py::new(
            py,
            PyAsgiReceive {
                body: Arc::new(Mutex::new(Some(body_bytes))),
                finished: finished_rx,
            },
        )?
        .into_any();
        let send = Py::new(
            py,
            PyAsgiSend {
                tx: send_tx,
                finished: finished_tx,
            },
        )?
        .into_any();

        let coro = instance
            .bind(py)
            .call1((scope.bind(py), receive.bind(py), send.bind(py)))?;
        py_bridge::schedule_task(py, event_loop, coro)
    });

    let (head_tx, head_rx) = oneshot::channel();
    let (body_tx, body_rx) = channel(16);
    spawn(pump(outcome?, send_rx, head_tx, body_tx));
    let (status, headers) = head_rx.await.map_err(|_| {
        PyRuntimeError::new_err("the ASGI app completed without sending a response")
    })??;
    let body = unfold(body_rx, |mut body_rx| async move {
        body_rx.recv().await.map(|chunk| (chunk, body_rx))
    });
    Ok(build_response(status, headers, Body::from_stream(body)))
}

type Head = (u16, Vec<(Bytes, Bytes)>);
type BodyChunk = Result<Bytes, std::io::Error>;

async fn pump(
    fut: py_bridge::PyTaskFuture,
    mut send_rx: Receiver<Py<PyAny>>,
    head_tx: oneshot::Sender<PyResult<Head>>,
    body_tx: Sender<BodyChunk>,
) {
    let mut head = Some(head_tx);
    let mut fut = pin!(fut);
    let result = loop {
        tokio::select! {
            Some(msg) = send_rx.recv() => forward(&mut head, &body_tx, msg).await,
            result = &mut fut => break result,
        }
    };
    while let Ok(msg) = send_rx.try_recv() {
        forward(&mut head, &body_tx, msg).await;
    }
    if let Err(err) = result {
        if let Some(head_tx) = head.take() {
            _ = head_tx.send(Err(err));
        } else {
            let failure = std::io::Error::other(err.to_string());
            _ = body_tx.send(Err(failure)).await;
        }
    }
}

async fn forward(
    head: &mut Option<oneshot::Sender<PyResult<Head>>>,
    body_tx: &Sender<BodyChunk>,
    msg: Py<PyAny>,
) {
    match Python::attach(|py| parse_response_message(py, msg.bind(py))) {
        ResponseMessage::Start(status, headers) => {
            if let Some(head_tx) = head.take() {
                _ = head_tx.send(Ok((status, headers)));
            }
        }
        ResponseMessage::Body(chunk, _) if !chunk.is_empty() => {
            _ = body_tx.send(Ok(chunk)).await;
        }
        _ => {}
    }
}

fn parse_response_message(py: Python<'_>, msg: &Bound<'_, PyAny>) -> ResponseMessage {
    let kind = msg
        .get_item(intern!(py, "type"))
        .ok()
        .and_then(|value| value.extract::<String>().ok())
        .unwrap_or_default();
    match kind.as_str() {
        "http.response.start" => {
            let status = msg
                .get_item(intern!(py, "status"))
                .ok()
                .and_then(|value| value.extract::<u16>().ok())
                .unwrap_or(200);
            let headers = msg
                .get_item(intern!(py, "headers"))
                .ok()
                .and_then(|value| value.extract::<Vec<(Vec<u8>, Vec<u8>)>>().ok())
                .unwrap_or_default()
                .into_iter()
                .map(|(name, value)| (Bytes::from(name), Bytes::from(value)))
                .collect();
            ResponseMessage::Start(status, headers)
        }
        "http.response.body" => {
            let body = msg
                .get_item(intern!(py, "body"))
                .ok()
                .and_then(|raw| python_bytes(py, &raw))
                .unwrap_or_default();
            let more_body = msg
                .get_item(intern!(py, "more_body"))
                .is_ok_and(|more| more.is_truthy().unwrap_or(false));
            ResponseMessage::Body(body, more_body)
        }
        _ => ResponseMessage::Other,
    }
}

fn build_response(status: u16, headers: Vec<(Bytes, Bytes)>, body: Body) -> Response {
    let mut header_map = HeaderMap::new();
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::try_from(name.as_ref()),
            HeaderValue::try_from(value.as_ref()),
        ) {
            header_map.append(name, value);
        } else {
            error!("the ASGI app sent an invalid response header");
        }
    }
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = Response::builder().status(status);
    if let Some(parts) = builder.headers_mut() {
        *parts = header_map;
    }
    builder.body(body).unwrap_or_else(|_| plain_server_error())
}

fn build_scope(py: Python<'_>, parts: &Parts, server: EventLoop) -> PyResult<Py<PyDict>> {
    let scope = PyDict::new(py);
    scope.set_item(intern!(py, "type"), intern!(py, "http"))?;
    let asgi = PyDict::new(py);
    asgi.set_item(intern!(py, "version"), intern!(py, "3.0"))?;
    asgi.set_item(intern!(py, "spec_version"), intern!(py, "2.3"))?;
    scope.set_item(intern!(py, "asgi"), asgi)?;
    scope.set_item(intern!(py, "http_version"), intern!(py, "1.1"))?;
    scope.set_item(intern!(py, "method"), parts.method.as_str())?;
    scope.set_item(intern!(py, "scheme"), intern!(py, "http"))?;
    scope.set_item(
        intern!(py, "path"),
        crate::request::decode_path(parts.uri.path()),
    )?;
    scope.set_item(
        intern!(py, "raw_path"),
        PyBytes::new(py, parts.uri.path().as_bytes()),
    )?;
    scope.set_item(
        intern!(py, "query_string"),
        PyBytes::new(py, parts.uri.query().unwrap_or("").as_bytes()),
    )?;
    scope.set_item(intern!(py, "root_path"), "")?;
    let headers = PyList::empty(py);
    for (name, value) in &parts.headers {
        headers.append((
            PyBytes::new(py, name.as_str().as_bytes()),
            PyBytes::new(py, value.as_bytes()),
        ))?;
    }
    scope.set_item(intern!(py, "headers"), headers)?;
    scope.set_item(intern!(py, "client"), py.None())?;
    scope.set_item(intern!(py, "server"), py.None())?;
    if let Some(app) = server.server().app() {
        scope.set_item(intern!(py, "app"), app.bind(py))?;
    }
    scope.set_item(intern!(py, "state"), PyDict::new(py))?;
    Ok(scope.unbind())
}
