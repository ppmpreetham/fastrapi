//! This module contains the `WebSocket` Python class, which is compatible with starlette.
//! The class accepts the connection, sends and receives text, bytes, and JSON messages, and closes the connection.

use crate::request::PyHTTPConnection;
use crate::response_api::JSON_DUMPS;
use crate::response_convert::convert_auto_response;
use crate::stream::SlotLease;
use axum::body::Body;
use axum::http::header::SEC_WEBSOCKET_PROTOCOL;
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::response::Response;
use bytes::{BufMut, Bytes, BytesMut};
use fastrapi_runtime::py_bridge;
use parking_lot::Mutex;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict, PyString};
use std::str::from_utf8;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

use crate::exceptions::PyWebSocketDisconnect;
use fastrapi_pyutil::json::{json_to_py_object, write_py_json};
use pyo3::exceptions::{PyKeyError, PyRuntimeError, PyStopAsyncIteration, PyValueError};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

pub enum WSMessage {
    Text(Bytes),
    Binary(Bytes),
    Close(u16, String),
}

type SharedRx = Arc<Mutex<Option<mpsc::Receiver<WSMessage>>>>;

fn disconnected() -> PyErr {
    disconnected_with_code(1000, String::new())
}

pub enum WSDecision {
    Accept(HeaderMap),
    Reject(u16, String),
    Deny(Response),
}

type RawHeaders = Vec<(Vec<u8>, Vec<u8>)>;
type DenialParts = (u16, RawHeaders, Vec<u8>);

fn accept_headers(subprotocol: Option<&str>, headers: Option<RawHeaders>) -> PyResult<HeaderMap> {
    let invalid = |err: &dyn std::fmt::Display| PyValueError::new_err(err.to_string());
    let mut accepted = HeaderMap::new();
    if let Some(subprotocol) = subprotocol {
        let value = HeaderValue::from_str(subprotocol).map_err(|err| invalid(&err))?;
        accepted.insert(SEC_WEBSOCKET_PROTOCOL, value);
    }
    for (name, value) in headers.into_iter().flatten() {
        accepted.append(
            HeaderName::from_bytes(&name).map_err(|err| invalid(&err))?,
            HeaderValue::from_bytes(&value).map_err(|err| invalid(&err))?,
        );
    }
    Ok(accepted)
}

fn denial_response((status, headers, body): DenialParts) -> PyResult<Response> {
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from(body))
        .map_err(|e| PyValueError::new_err(e.to_string()))
}

fn disconnected_with_code(code: u16, reason: String) -> PyErr {
    Python::attach(|py| {
        PyErr::from_type(
            py.get_type::<PyWebSocketDisconnect>(),
            (code, (!reason.is_empty()).then_some(reason)),
        )
    })
}

/// Starlette reads `message["text"]` or `message["bytes"]`, so a frame of the other type is a `KeyError`.
fn wrong_frame(key: &str) -> PyErr {
    PyKeyError::new_err(key.to_owned())
}

fn binary_mode(mode: &str) -> PyResult<bool> {
    match mode {
        "text" => Ok(false),
        "binary" => Ok(true),
        _ => Err(PyRuntimeError::new_err(
            "The \"mode\" argument should be \"text\" or \"binary\".",
        )),
    }
}

/// Starlette serialises with `json.dumps`, which writes `NaN` and `Infinity` where the native writer raises.
fn json_payload(py: Python<'_>, data: &Bound<'_, PyAny>) -> PyResult<Bytes> {
    let mut buf = BytesMut::new();
    match write_py_json(py, data, &mut (&mut buf).writer()) {
        Err(err) if err.is_instance_of::<PyValueError>(py) => {
            let kwargs = PyDict::new(py);
            kwargs.set_item(pyo3::intern!(py, "ensure_ascii"), false)?;
            kwargs.set_item(pyo3::intern!(py, "separators"), (",", ":"))?;
            let text: String = JSON_DUMPS
                .get(py)?
                .call((data,), Some(&kwargs))?
                .extract()?;
            Ok(Bytes::from(text))
        }
        result => result.map(|()| buf.freeze()),
    }
}

fn parse_json_bytes(bytes: Bytes) -> PyResult<Py<PyAny>> {
    let mut json_buf = Vec::from(bytes);
    let value = simd_json::to_owned_value(&mut json_buf)
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(Python::attach(|py| json_to_py_object(py, &value)))
}

async fn next_message(rx_arc: SharedRx) -> PyResult<WSMessage> {
    let Some(mut lease) = SlotLease::take(&rx_arc) else {
        return Err(PyRuntimeError::new_err(
            "Concurrent receive operations are not supported",
        ));
    };
    let received = match lease.value() {
        Some(rx) => rx.recv().await,
        None => None,
    };
    drop(lease);
    received.ok_or_else(disconnected)
}

#[pyclass(name = "WebSocket", extends = PyHTTPConnection)]
pub struct PyWebSocket {
    pub tx: mpsc::Sender<WSMessage>,
    pub rx: SharedRx,
    pub is_connected: Arc<AtomicBool>,
    pub decision: Arc<Mutex<Option<oneshot::Sender<WSDecision>>>>,
    pub handshake_state: Arc<AtomicU8>,
    pub connect_delivered: Arc<AtomicBool>,
    pub denial: Arc<Mutex<Option<DenialParts>>>,
}

fastrapi_pyutil::cached_py_import!(WEBSOCKET_STATE, "starlette.websockets", "WebSocketState");

fn websocket_state(py: Python<'_>, value: u8) -> PyResult<Py<PyAny>> {
    Ok(WEBSOCKET_STATE.get(py)?.call1((value,))?.unbind())
}

fn asgi_message(py: Python<'_>, message: WSMessage) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    match message {
        WSMessage::Text(bytes) => {
            dict.set_item(pyo3::intern!(py, "type"), "websocket.receive")?;
            dict.set_item(
                "text",
                from_utf8(&bytes).map_err(|e| PyValueError::new_err(e.to_string()))?,
            )?;
        }
        WSMessage::Binary(data) => {
            dict.set_item(pyo3::intern!(py, "type"), "websocket.receive")?;
            dict.set_item(pyo3::intern!(py, "bytes"), PyBytes::new(py, &data))?;
        }
        WSMessage::Close(code, reason) => {
            dict.set_item(pyo3::intern!(py, "type"), "websocket.disconnect")?;
            dict.set_item(pyo3::intern!(py, "code"), code)?;
            dict.set_item(pyo3::intern!(py, "reason"), reason)?;
        }
    }
    Ok(dict.into_any().unbind())
}

impl PyWebSocket {
    /// Starlette's `receive_*` need an accepted connection (`application_state` CONNECTED).
    fn ensure_receivable(&self) -> PyResult<()> {
        if self.handshake_state.load(Ordering::Acquire) == 1 {
            return Ok(());
        }
        Err(PyRuntimeError::new_err(
            "WebSocket is not connected. Need to call \"accept\" first.",
        ))
    }

    /// Starlette's `send_*` raise before `accept()` and after a close or denial.
    fn ensure_sendable(&self) -> PyResult<()> {
        Err(PyRuntimeError::new_err(
            match self.handshake_state.load(Ordering::Acquire) {
                1 => return Ok(()),
                0 => {
                    "Expected ASGI message \"websocket.accept\", \"websocket.close\" or \"websocket.http.response.start\", but got 'websocket.send'"
                }
                4 => {
                    "Expected ASGI message \"websocket.http.response.body\", but got 'websocket.send'"
                }
                _ => "Cannot call \"send\" once a close message has been sent.",
            },
        ))
    }

    pub fn deny<'py>(&self, py: Python<'py>, response: Response) -> PyResult<Bound<'py, PyAny>> {
        if self
            .handshake_state
            .compare_exchange(0, 4, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(PyRuntimeError::new_err(
                "The denial response can only be sent before the WebSocket is accepted",
            ));
        }
        if let Some(sender) = self.decision.lock().take() {
            let _ = sender.send(WSDecision::Deny(response));
        }
        py_bridge::future_into_py(py, async move { Ok(()) })
    }
}

#[pymethods]
impl PyWebSocket {
    #[pyo3(signature = (subprotocol=None, headers=None))]
    fn accept<'py>(
        &self,
        py: Python<'py>,
        subprotocol: Option<&str>,
        headers: Option<RawHeaders>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let accepted = accept_headers(subprotocol, headers)?;
        let decision = self.decision.clone();
        let state = self.handshake_state.clone();
        py_bridge::future_into_py(py, async move {
            if state
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
                && let Some(sender) = decision.lock().take()
            {
                let _ = sender.send(WSDecision::Accept(accepted));
            }
            Ok(())
        })
    }

    fn send_text<'py>(&self, py: Python<'py>, data: String) -> PyResult<Bound<'py, PyAny>> {
        self.ensure_sendable()?;
        let tx = self.tx.clone();
        py_bridge::future_into_py(py, async move {
            tx.send(WSMessage::Text(Bytes::from(data)))
                .await
                .map_err(|_| closed())?;
            Ok(())
        })
    }

    fn send_bytes<'py>(&self, py: Python<'py>, data: Vec<u8>) -> PyResult<Bound<'py, PyAny>> {
        self.ensure_sendable()?;
        let tx = self.tx.clone();
        py_bridge::future_into_py(py, async move {
            tx.send(WSMessage::Binary(Bytes::from(data)))
                .await
                .map_err(|_| closed())?;
            Ok(())
        })
    }

    #[pyo3(signature = (data, mode="text"))]
    fn send_json<'py>(
        &self,
        py: Python<'py>,
        data: &Bound<'_, PyAny>,
        mode: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let binary = binary_mode(mode)?;
        let payload = json_payload(py, data)?;
        self.ensure_sendable()?;
        let message = if binary {
            WSMessage::Binary(payload)
        } else {
            WSMessage::Text(payload)
        };
        let tx = self.tx.clone();
        py_bridge::future_into_py(py, async move {
            tx.send(message).await.map_err(|_| closed())?;
            Ok(())
        })
    }

    fn receive_text<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.ensure_receivable()?;
        let rx = self.rx.clone();
        py_bridge::future_into_py(py, async move {
            match next_message(rx).await? {
                WSMessage::Text(bytes) => String::from_utf8(Vec::from(bytes))
                    .map_err(|e| PyValueError::new_err(e.to_string())),
                WSMessage::Binary(_) => Err(wrong_frame("text")),
                WSMessage::Close(code, reason) => Err(disconnected_with_code(code, reason)),
            }
        })
    }

    fn receive_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.ensure_receivable()?;
        let rx = self.rx.clone();
        py_bridge::future_into_py(py, async move {
            match next_message(rx).await? {
                WSMessage::Binary(data) => Ok(data),
                WSMessage::Text(_) => Err(wrong_frame("bytes")),
                WSMessage::Close(code, reason) => Err(disconnected_with_code(code, reason)),
            }
        })
    }

    #[pyo3(signature = (mode="text"))]
    fn receive_json<'py>(&self, py: Python<'py>, mode: &str) -> PyResult<Bound<'py, PyAny>> {
        let binary = binary_mode(mode)?;
        self.ensure_receivable()?;
        let rx = self.rx.clone();
        py_bridge::future_into_py(py, async move {
            match next_message(rx).await? {
                WSMessage::Text(bytes) if !binary => parse_json_bytes(bytes),
                WSMessage::Binary(bytes) if binary => parse_json_bytes(bytes),
                WSMessage::Text(_) => Err(wrong_frame("bytes")),
                WSMessage::Binary(_) => Err(wrong_frame("text")),
                WSMessage::Close(code, reason) => Err(disconnected_with_code(code, reason)),
            }
        })
    }

    fn send_denial_response<'py>(
        &self,
        py: Python<'py>,
        response: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let response = convert_auto_response(py, response);
        self.deny(py, response)
    }

    fn receive<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let connect = self.handshake_state.load(Ordering::Acquire) == 0
            && !self.connect_delivered.swap(true, Ordering::AcqRel);
        let rx = self.rx.clone();
        let connected = self.is_connected.clone();
        py_bridge::future_into_py(py, async move {
            if connect {
                return Python::attach(|py| -> PyResult<Py<PyAny>> {
                    let dict = PyDict::new(py);
                    dict.set_item(pyo3::intern!(py, "type"), "websocket.connect")?;
                    Ok(dict.into_any().unbind())
                });
            }
            let message = next_message(rx).await?;
            if matches!(message, WSMessage::Close(..)) {
                connected.store(false, Ordering::Release);
            }
            Python::attach(|py| asgi_message(py, message))
        })
    }

    fn send<'py>(
        &self,
        py: Python<'py>,
        message: &Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let kind: String = message
            .get_item(pyo3::intern!(py, "type"))?
            .ok_or_else(|| PyValueError::new_err("ASGI message has no 'type'"))?
            .extract()?;
        let field = |key: &str| -> PyResult<Option<Bound<'py, PyAny>>> {
            Ok(message.get_item(key)?.filter(|value| !value.is_none()))
        };
        match kind.as_str() {
            "websocket.accept" => {
                let subprotocol = field("subprotocol")?
                    .map(|value| value.extract::<String>())
                    .transpose()?;
                let headers = field("headers")?.map(|value| value.extract()).transpose()?;
                self.accept(py, subprotocol.as_deref(), headers)
            }
            "websocket.send" => {
                if let Some(text) = field("text")? {
                    self.send_text(py, text.extract()?)
                } else if let Some(data) = field("bytes")? {
                    self.send_bytes(py, data.extract()?)
                } else {
                    Err(PyValueError::new_err(
                        "websocket.send needs a 'text' or 'bytes' key",
                    ))
                }
            }
            "websocket.http.response.start" => {
                let status = field("status")?.map_or(Ok(200), |status| status.extract())?;
                let headers = field("headers")?
                    .map(|headers| headers.extract::<RawHeaders>())
                    .transpose()?
                    .unwrap_or_default();
                *self.denial.lock() = Some((status, headers, Vec::new()));
                py_bridge::future_into_py(py, async move { Ok(()) })
            }
            "websocket.http.response.body" => {
                let body: Vec<u8> = field("body")?
                    .map(|body| body.extract())
                    .transpose()?
                    .unwrap_or_default();
                let more_body = field("more_body")?
                    .map(|more| more.is_truthy())
                    .transpose()?
                    .unwrap_or(false);
                let mut denial = self.denial.lock();
                let parts = denial.as_mut().ok_or_else(|| {
                    PyRuntimeError::new_err(
                        "websocket.http.response.body sent before websocket.http.response.start",
                    )
                })?;
                parts.2.extend_from_slice(&body);
                if more_body {
                    return py_bridge::future_into_py(py, async move { Ok(()) });
                }
                let response = denial_response(denial.take().unwrap_or_default())?;
                drop(denial);
                self.deny(py, response)
            }
            "websocket.close" => {
                let code = field("code")?.map_or(Ok(1000), |code| code.extract())?;
                let reason = field("reason")?
                    .map(|reason| reason.extract())
                    .transpose()?;
                self.close(py, code, reason)
            }
            other => Err(PyRuntimeError::new_err(format!(
                "Unexpected ASGI message '{other}'"
            ))),
        }
    }

    #[pyo3(signature = (code = 1000, reason = None))]
    fn close<'py>(
        &self,
        py: Python<'py>,
        code: u16,
        reason: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let tx = self.tx.clone();
        let decision = self.decision.clone();
        let state = self.handshake_state.clone();
        let reason = reason.unwrap_or_default();
        py_bridge::future_into_py(py, async move {
            if state
                .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
                && let Some(sender) = decision.lock().take()
            {
                let _ = sender.send(WSDecision::Reject(code, reason));
                return Ok(());
            }
            _ = state.compare_exchange(1, 3, Ordering::AcqRel, Ordering::Acquire);
            tx.send(WSMessage::Close(code, reason))
                .await
                .map_err(|_| closed())?;
            Ok(())
        })
    }

    #[getter]
    fn client_state(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let value = if self.handshake_state.load(Ordering::Acquire) == 0
            && !self.connect_delivered.load(Ordering::Acquire)
        {
            0
        } else if self.is_connected.load(Ordering::Acquire) {
            1
        } else {
            2
        };
        websocket_state(py, value)
    }

    #[getter]
    fn application_state(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let value = match self.handshake_state.load(Ordering::Acquire) {
            0 => 0,
            1 => 1,
            4 => 3,
            _ => 2,
        };
        websocket_state(py, value)
    }

    fn iter_text(self_: Py<Self>, py: Python<'_>) -> PyResult<Py<WSIterator>> {
        Py::new(py, WSIterator::new(self_, WsPayload::Text))
    }

    fn iter_json(self_: Py<Self>, py: Python<'_>) -> PyResult<Py<WSIterator>> {
        Py::new(py, WSIterator::new(self_, WsPayload::Json))
    }

    fn iter_bytes(self_: Py<Self>, py: Python<'_>) -> PyResult<Py<WSIterator>> {
        Py::new(py, WSIterator::new(self_, WsPayload::Bytes))
    }
}

fn closed() -> PyErr {
    PyRuntimeError::new_err("WebSocket closed")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WsPayload {
    Text,
    Bytes,
    Json,
}

/// The asynchronous iterator for `iter_text()` and `iter_json()`.
#[pyclass(name = "WSIterator")]
pub struct WSIterator {
    ws: Py<PyWebSocket>,
    payload: WsPayload,
}

impl WSIterator {
    const fn new(ws: Py<PyWebSocket>, payload: WsPayload) -> Self {
        Self { ws, payload }
    }
}

#[pymethods]
impl WSIterator {
    const fn __aiter__(self_: Py<Self>) -> Py<Self> {
        self_
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ws = self.ws.clone_ref(py);
        let payload = self.payload;

        py_bridge::future_into_py(py, async move {
            let rx = Python::attach(|py| {
                let ws = ws.borrow(py);
                ws.ensure_receivable().map(|()| ws.rx.clone())
            })?;
            match next_message(rx).await {
                Ok(WSMessage::Text(bytes)) if payload == WsPayload::Text => {
                    let text =
                        from_utf8(&bytes).map_err(|e| PyValueError::new_err(e.to_string()))?;
                    Ok(Python::attach(|py| {
                        PyString::new(py, text).into_any().unbind()
                    }))
                }
                Ok(WSMessage::Binary(data)) if payload == WsPayload::Bytes => {
                    Ok(Python::attach(|py| {
                        PyBytes::new(py, &data).into_any().unbind()
                    }))
                }
                Ok(WSMessage::Text(_)) if payload == WsPayload::Bytes => Err(wrong_frame("bytes")),
                Ok(WSMessage::Text(bytes)) => parse_json_bytes(bytes),
                Ok(WSMessage::Binary(_)) => Err(wrong_frame("text")),
                Ok(WSMessage::Close(..)) => Err(PyStopAsyncIteration::new_err(())),
                Err(err)
                    if Python::attach(|py| err.is_instance_of::<PyWebSocketDisconnect>(py)) =>
                {
                    Err(PyStopAsyncIteration::new_err(()))
                }
                Err(err) => Err(err),
            }
        })
    }
}
