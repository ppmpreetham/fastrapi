//! This module contains the `WebSocket` Python class, which is compatible with starlette.
//! The class accepts the connection, sends and receives text, bytes, and JSON messages, and closes the connection.

use crate::request::PyHTTPConnection;
use crate::response_convert::convert_auto_response;
use axum::body::Body;
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
use pyo3::exceptions::{PyRuntimeError, PyStopAsyncIteration, PyTypeError, PyValueError};
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
    Accept,
    Reject(u16),
    Deny(Response),
}

type DenialParts = (u16, Vec<(Vec<u8>, Vec<u8>)>, Vec<u8>);

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

fn expected(expected: &str, got: &str) -> PyErr {
    PyTypeError::new_err(format!("Expected {expected}, got {got}"))
}

fn parse_json_bytes(bytes: Bytes) -> PyResult<Py<PyAny>> {
    let mut json_buf = Vec::from(bytes);
    let value = simd_json::to_owned_value(&mut json_buf)
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(Python::attach(|py| json_to_py_object(py, &value)))
}

async fn next_message(rx_arc: SharedRx) -> PyResult<WSMessage> {
    let mut rx = {
        let mut guard = rx_arc.lock();
        guard.take().ok_or_else(|| {
            PyRuntimeError::new_err("Concurrent receive operations are not supported")
        })?
    };

    let received = rx.recv().await;

    if let Some(mut guard) = rx_arc.try_lock() {
        *guard = Some(rx);
    } else {
        tracing::error!("websocket receiver lock poisoned");
    }

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
            dict.set_item("type", "websocket.receive")?;
            dict.set_item(
                "text",
                from_utf8(&bytes).map_err(|e| PyValueError::new_err(e.to_string()))?,
            )?;
        }
        WSMessage::Binary(data) => {
            dict.set_item("type", "websocket.receive")?;
            dict.set_item("bytes", PyBytes::new(py, &data))?;
        }
        WSMessage::Close(code, reason) => {
            dict.set_item("type", "websocket.disconnect")?;
            dict.set_item("code", code)?;
            dict.set_item("reason", reason)?;
        }
    }
    Ok(dict.into_any().unbind())
}

impl PyWebSocket {
    fn deny<'py>(&self, py: Python<'py>, response: Response) -> PyResult<Bound<'py, PyAny>> {
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
    fn accept<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let decision = self.decision.clone();
        let state = self.handshake_state.clone();
        py_bridge::future_into_py(py, async move {
            if state
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
                && let Some(sender) = decision.lock().take()
            {
                let _ = sender.send(WSDecision::Accept);
            }
            Ok(())
        })
    }

    fn send_text<'py>(&self, py: Python<'py>, data: String) -> PyResult<Bound<'py, PyAny>> {
        let tx = self.tx.clone();
        py_bridge::future_into_py(py, async move {
            tx.send(WSMessage::Text(Bytes::from(data)))
                .await
                .map_err(|_| closed())?;
            Ok(())
        })
    }

    fn send_bytes<'py>(&self, py: Python<'py>, data: Vec<u8>) -> PyResult<Bound<'py, PyAny>> {
        let tx = self.tx.clone();
        py_bridge::future_into_py(py, async move {
            tx.send(WSMessage::Binary(Bytes::from(data)))
                .await
                .map_err(|_| closed())?;
            Ok(())
        })
    }

    fn send_json<'py>(
        &self,
        py: Python<'py>,
        data: &Bound<'_, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mut buf = BytesMut::new();
        let mut writer = (&mut buf).writer();
        write_py_json(py, data, &mut writer)?;
        let payload = buf.freeze();
        let tx = self.tx.clone();
        py_bridge::future_into_py(py, async move {
            tx.send(WSMessage::Text(payload))
                .await
                .map_err(|_| closed())?;
            Ok(())
        })
    }

    fn receive_text<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = self.rx.clone();
        py_bridge::future_into_py(py, async move {
            match next_message(rx).await? {
                WSMessage::Text(bytes) => String::from_utf8(Vec::from(bytes))
                    .map_err(|e| PyValueError::new_err(e.to_string())),
                WSMessage::Binary(_) => Err(expected("text", "binary")),
                WSMessage::Close(code, reason) => Err(disconnected_with_code(code, reason)),
            }
        })
    }

    fn receive_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = self.rx.clone();
        py_bridge::future_into_py(py, async move {
            match next_message(rx).await? {
                WSMessage::Binary(data) => Ok(data),
                WSMessage::Text(_) => Err(expected("binary", "text")),
                WSMessage::Close(code, reason) => Err(disconnected_with_code(code, reason)),
            }
        })
    }

    fn receive_json<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = self.rx.clone();
        py_bridge::future_into_py(py, async move {
            match next_message(rx).await? {
                WSMessage::Text(bytes) => parse_json_bytes(bytes),
                WSMessage::Binary(_) => Err(expected("text", "binary")),
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
                    dict.set_item("type", "websocket.connect")?;
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
            .get_item("type")?
            .ok_or_else(|| PyValueError::new_err("ASGI message has no 'type'"))?
            .extract()?;
        let field = |key: &str| -> PyResult<Option<Bound<'py, PyAny>>> {
            Ok(message.get_item(key)?.filter(|value| !value.is_none()))
        };
        match kind.as_str() {
            "websocket.accept" => self.accept(py),
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
                    .map(|headers| headers.extract::<Vec<(Vec<u8>, Vec<u8>)>>())
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
        py_bridge::future_into_py(py, async move {
            if state
                .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
                && let Some(sender) = decision.lock().take()
            {
                let _ = sender.send(WSDecision::Reject(code));
                return Ok(());
            }
            _ = state.compare_exchange(1, 3, Ordering::AcqRel, Ordering::Acquire);
            tx.send(WSMessage::Close(code, reason.unwrap_or_default()))
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
            let rx = Python::attach(|py| ws.borrow(py).rx.clone());
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
                Ok(WSMessage::Text(_)) if payload == WsPayload::Bytes => {
                    Err(expected("binary", "text"))
                }
                Ok(WSMessage::Text(bytes)) => parse_json_bytes(bytes),
                Ok(WSMessage::Binary(_)) => Err(expected("text", "binary")),
                Ok(WSMessage::Close(..)) | Err(_) => Err(PyStopAsyncIteration::new_err(())),
            }
        })
    }
}
