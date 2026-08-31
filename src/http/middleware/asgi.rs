use axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::{HeaderName, HeaderValue, StatusCode, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
};
use pyo3::{
    exceptions::PyRuntimeError,
    intern,
    prelude::*,
    types::{PyAny, PyByteArray, PyBytes, PyDict, PyList},
};
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
    locals: rsloop::rust_async::TaskLocals,
}

#[pyclass(
    frozen,
    name = "ScopeDownstream",
    module = "fastrapi.middleware",
    skip_from_py_object
)]
pub struct PyScopeDownstream;

/// yields the buffered request body once, then `http.disconnect` forever.
#[pyclass(
    frozen,
    name = "Receive",
    module = "fastrapi.middleware",
    skip_from_py_object
)]
pub struct PyAsgiReceive {
    body: std::sync::Arc<tokio::sync::Mutex<Option<bytes::Bytes>>>,
}

/// collects the `http.response.*` messages the middleware chain sends.
#[pyclass(
    frozen,
    name = "Send",
    module = "fastrapi.middleware",
    skip_from_py_object
)]
pub struct PyAsgiSend {
    tx: tokio::sync::mpsc::Sender<Py<PyAny>>,
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
            return rsloop::rust_async::future_into_py(py, async move {
                Err::<Py<PyAny>, _>(PyRuntimeError::new_err(
                    "asgi middleware called the downstream app with a scope that has no request attached",
                ))
            });
        };

        let borrowed = handle.borrow();
        let downstream = PyDownstreamASGI {
            next: borrowed.next.clone(),
            head: borrowed.head.clone(),
            locals: borrowed.locals.clone(),
        };
        let receive = receive.clone().unbind();
        let send = send.clone().unbind();

        rsloop::rust_async::future_into_py(py, async move {
            run_downstream(downstream, receive, send).await
        })
    }
}

/// runs the rest of the axum stack, feeding the body from the `receive` the
/// middleware passed in and emitting the response through the `send` it passed.
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

/// drives the middleware-supplied `receive` callable and reassembles the body.
async fn drain_receive(
    locals: &rsloop::rust_async::TaskLocals,
    receive: &Py<PyAny>,
) -> PyResult<bytes::Bytes> {
    let mut buf = bytes::BytesMut::new();
    loop {
        let msg: Py<PyAny> = {
            let fut = Python::attach(|py| -> PyResult<_> {
                let awaitable = receive.bind(py).call0()?;
                rsloop::rust_async::into_future_with_locals(locals, awaitable)
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

/// (`body`, `more_body`) from an asgi `http.request` message; `None` on disconnect.
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
        return Some(bytes::Bytes::copy_from_slice(bytes.as_bytes()));
    }
    if let Ok(bytearray) = obj.cast::<PyByteArray>() {
        return Some(bytes::Bytes::from(bytearray.to_vec()));
    }
    let as_bytes = obj.call_method0(intern!(py, "__bytes__")).ok()?;
    let bytes = as_bytes.cast::<PyBytes>().ok()?;
    Some(bytes::Bytes::copy_from_slice(bytes.as_bytes()))
}

/// emits `http.response.start` + `http.response.body` through the middleware's send.
async fn send_response(
    locals: &rsloop::rust_async::TaskLocals,
    send: &Py<PyAny>,
    resp: Response,
) -> PyResult<()> {
    let (parts, body) = resp.into_parts();
    let body_bytes = to_bytes(body, usize::MAX).await.unwrap_or_default();

    let start = Python::attach(|py| -> PyResult<Py<PyAny>> {
        let msg = PyDict::new(py);
        msg.set_item(intern!(py, "type"), intern!(py, "http.response.start"))?;
        msg.set_item(intern!(py, "status"), parts.status.as_u16())?;
        let headers = PyList::empty(py);
        for (name, value) in parts.headers.iter() {
            headers.append((
                PyBytes::new(py, name.as_str().as_bytes()),
                PyBytes::new(py, value.as_bytes()),
            ))?;
        }
        msg.set_item(intern!(py, "headers"), headers)?;
        Ok(msg.unbind().into_any())
    })?;
    asgi_call(locals, send, start).await?;

    let body_msg = Python::attach(|py| -> PyResult<Py<PyAny>> {
        let msg = PyDict::new(py);
        msg.set_item(intern!(py, "type"), intern!(py, "http.response.body"))?;
        msg.set_item(intern!(py, "body"), PyBytes::new(py, &body_bytes))?;
        msg.set_item(intern!(py, "more_body"), false)?;
        Ok(msg.unbind().into_any())
    })?;
    asgi_call(locals, send, body_msg).await
}

/// awaits one `send(message)` call on the python loop.
async fn asgi_call(
    locals: &rsloop::rust_async::TaskLocals,
    target: &Py<PyAny>,
    message: Py<PyAny>,
) -> PyResult<()> {
    let fut = Python::attach(|py| -> PyResult<_> {
        let awaitable = target.bind(py).call1((message.bind(py),))?;
        rsloop::rust_async::into_future_with_locals(locals, awaitable)
    })?;
    fut.await?;
    Ok(())
}

#[pymethods]
impl PyAsgiReceive {
    fn __call__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let body = self.body.clone();
        rsloop::rust_async::future_into_py(py, async move {
            let mut guard = body.lock().await;
            Python::attach(|py| {
                let msg = PyDict::new(py);
                match guard.take() {
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
        let message = message.clone().unbind();
        rsloop::rust_async::future_into_py(py, async move {
            tx.send(message)
                .await
                .map_err(|_| PyRuntimeError::new_err("asgi send channel closed"))
        })
    }
}

/// collected `http.response.*` messages, parsed.
enum ResponseMessage {
    Start(u16, Vec<(bytes::Bytes, bytes::Bytes)>),
    Body(bytes::Bytes),
    Other,
}

pub(crate) async fn run_asgi_request(
    instance: std::sync::Arc<Py<PyAny>>,
    req: Request,
    next: Next,
    async_loop: std::sync::Arc<Py<PyAny>>,
    max_body_size: usize,
) -> Response {
    let (parts, body) = req.into_parts();
    let body_bytes = to_bytes(body, max_body_size).await.unwrap_or_default();
    let (send_tx, mut send_rx) = tokio::sync::mpsc::channel::<Py<PyAny>>(16);
    let close_tx = send_tx.clone();

    let outcome = Python::attach(|py| -> PyResult<_> {
        let locals = rsloop::rust_async::TaskLocals::new(async_loop.bind(py).clone());
        let scope = build_scope(py, &parts)?;
        let handle = Py::new(
            py,
            PyDownstreamASGI {
                next,
                head: parts,
                locals: locals.clone(),
            },
        )?
        .into_any();
        scope
            .bind(py)
            .set_item(intern!(py, DOWNSTREAM_KEY), &handle)?;

        let receive = Py::new(
            py,
            PyAsgiReceive {
                body: std::sync::Arc::new(tokio::sync::Mutex::new(Some(body_bytes))),
            },
        )?
        .into_any();
        let send = Py::new(py, PyAsgiSend { tx: send_tx })?.into_any();

        let coro = instance
            .bind(py)
            .call1((scope.bind(py), receive.bind(py), send.bind(py)))?;
        rsloop::rust_async::into_future_with_locals(&locals, coro)
    });

    match outcome {
        Ok(fut) => {
            if let Err(err) = fut.await {
                Python::attach(|py| err.print(py));
                return middleware_error_response();
            }
        }
        Err(err) => {
            Python::attach(|py| err.print(py));
            return middleware_error_response();
        }
    }

    drop(close_tx);
    let mut messages = Vec::new();
    while let Some(msg) = send_rx.recv().await {
        messages.push(Python::attach(|py| {
            parse_response_message(py, msg.bind(py))
        }));
    }
    collect_response(messages)
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
                .map(|(name, value)| (bytes::Bytes::from(name), bytes::Bytes::from(value)))
                .collect();
            ResponseMessage::Start(status, headers)
        }
        "http.response.body" => {
            let body = msg
                .get_item(intern!(py, "body"))
                .ok()
                .and_then(|raw| python_bytes(py, &raw))
                .unwrap_or_default();
            ResponseMessage::Body(body)
        }
        _ => ResponseMessage::Other,
    }
}

fn collect_response(messages: Vec<ResponseMessage>) -> Response {
    let mut status = None;
    let mut headers = axum::http::HeaderMap::new();
    let mut body = bytes::BytesMut::new();

    for message in messages {
        match message {
            ResponseMessage::Start(msg_status, msg_headers) => {
                status.get_or_insert(msg_status);
                for (name, value) in msg_headers {
                    match (
                        HeaderName::try_from(name.as_ref()),
                        HeaderValue::try_from(value.as_ref()),
                    ) {
                        (Ok(name), Ok(value)) => {
                            headers.append(name, value);
                        }
                        _ => error!("asgi middleware sent an invalid response header"),
                    }
                }
            }
            ResponseMessage::Body(chunk) => body.extend(chunk),
            ResponseMessage::Other => {}
        }
    }

    let Some(status) = status else {
        error!("asgi middleware completed without sending a response");
        return middleware_error_response();
    };

    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = Response::builder().status(status);
    if let Some(parts) = builder.headers_mut() {
        *parts = headers;
    }
    builder
        .body(Body::from(body.freeze()))
        .unwrap_or_else(|_| middleware_error_response())
}

fn middleware_error_response() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Middleware Execution Error",
    )
        .into_response()
}

fn build_scope(py: Python<'_>, parts: &Parts) -> PyResult<Py<PyDict>> {
    let scope = PyDict::new(py);
    scope.set_item(intern!(py, "type"), intern!(py, "http"))?;
    let asgi = PyDict::new(py);
    asgi.set_item(intern!(py, "version"), intern!(py, "3.0"))?;
    asgi.set_item(intern!(py, "spec_version"), intern!(py, "2.3"))?;
    scope.set_item(intern!(py, "asgi"), asgi)?;
    scope.set_item(intern!(py, "http_version"), intern!(py, "1.1"))?;
    scope.set_item(intern!(py, "method"), parts.method.as_str())?;
    scope.set_item(intern!(py, "scheme"), intern!(py, "http"))?;
    scope.set_item(intern!(py, "path"), parts.uri.path())?;
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
    for (name, value) in parts.headers.iter() {
        headers.append((
            PyBytes::new(py, name.as_str().as_bytes()),
            PyBytes::new(py, value.as_bytes()),
        ))?;
    }
    scope.set_item(intern!(py, "headers"), headers)?;
    scope.set_item(intern!(py, "client"), py.None())?;
    scope.set_item(intern!(py, "server"), py.None())?;
    if let Some(app) = crate::globals::serve_app() {
        scope.set_item(intern!(py, "app"), app.bind(py))?;
    }
    scope.set_item(intern!(py, "state"), PyDict::new(py))?;
    Ok(scope.unbind())
}
