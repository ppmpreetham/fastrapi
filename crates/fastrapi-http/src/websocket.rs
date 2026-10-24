//! This module contains the `WebSocket` Python class, which is compatible with starlette.
//! The class accepts the connection, sends and receives text, bytes, and JSON messages, and closes the connection.

use bytes::{BufMut, Bytes, BytesMut};
use fastrapi_pyutil::imports::{STARLETTE_HEADERS, TYPES};
use fastrapi_runtime::py_bridge;
use parking_lot::Mutex;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyString};
use std::str::from_utf8;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

use crate::exceptions::PyWebSocketDisconnect;
use fastrapi_pyutil::json::{json_to_py_object, write_py_json};
use pyo3::exceptions::{PyRuntimeError, PyStopAsyncIteration, PyTypeError, PyValueError};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

fastrapi_pyutil::cached_py_import!(
    WS_STARLETTE_QUERY_PARAMS,
    "starlette.datastructures",
    "QueryParams"
);

pub enum WSMessage {
    Text(Bytes),
    Binary(Bytes),
    Close(u16),
}

type SharedRx = Arc<Mutex<Option<mpsc::Receiver<WSMessage>>>>;

fn disconnected() -> PyErr {
    disconnected_with_code(1000)
}

pub enum WSDecision {
    Accept,
    Reject(u16),
}

fn disconnected_with_code(code: u16) -> PyErr {
    Python::attach(|py| {
        PyErr::from_type(
            py.get_type::<PyWebSocketDisconnect>(),
            (code, Option::<String>::None),
        )
    })
}

fn expected(expected: &str, got: &str) -> PyErr {
    PyTypeError::new_err(format!("Expected {expected}, got {got}"))
}

fn parse_json_bytes(bytes: &Bytes) -> PyResult<Py<PyAny>> {
    let mut json_buf = bytes.to_vec();
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

/// A base class for incoming HTTP connections, that is used to provide
/// any functionality that is common to both `Request` and `WebSocket`.
#[pyclass(name = "WebSocket")]
pub struct PyWebSocket {
    pub tx: mpsc::Sender<WSMessage>,
    pub rx: SharedRx,
    pub is_connected: Arc<AtomicBool>,
    pub scope: Option<Py<PyDict>>,
    pub decision: Arc<Mutex<Option<oneshot::Sender<WSDecision>>>>,
    pub handshake_state: Arc<AtomicU8>,
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
                WSMessage::Text(bytes) => String::from_utf8(bytes.to_vec())
                    .map_err(|e| PyValueError::new_err(e.to_string())),
                WSMessage::Binary(_) => Err(expected("text", "binary")),
                WSMessage::Close(code) => Err(disconnected_with_code(code)),
            }
        })
    }

    fn receive_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = self.rx.clone();
        py_bridge::future_into_py(py, async move {
            match next_message(rx).await? {
                WSMessage::Binary(data) => Ok(data.to_vec()),
                WSMessage::Text(_) => Err(expected("binary", "text")),
                WSMessage::Close(code) => Err(disconnected_with_code(code)),
            }
        })
    }

    fn receive_json<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = self.rx.clone();
        py_bridge::future_into_py(py, async move {
            match next_message(rx).await? {
                WSMessage::Text(bytes) => parse_json_bytes(&bytes),
                WSMessage::Binary(_) => Err(expected("text", "binary")),
                WSMessage::Close(code) => Err(disconnected_with_code(code)),
            }
        })
    }

    #[pyo3(signature = (code = 1000))]
    fn close<'py>(&self, py: Python<'py>, code: u16) -> PyResult<Bound<'py, PyAny>> {
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
            tx.send(WSMessage::Close(code))
                .await
                .map_err(|_| closed())?;
            Ok(())
        })
    }

    #[getter]
    fn client_state(&self) -> u8 {
        if self.is_connected.load(Ordering::Relaxed) {
            1 // Connected
        } else {
            3 // Disconnected
        }
    }

    #[getter]
    fn headers(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        starlette(STARLETTE_HEADERS.get(py)?, "raw", &raw_headers(self, py))
    }

    #[getter]
    fn query_params(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        starlette(
            WS_STARLETTE_QUERY_PARAMS.get(py)?,
            "query_string",
            &scope_value(self.scope.as_ref(), py, "query_string"),
        )
    }

    #[getter]
    fn state(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        if let Some(scope) = &self.scope {
            let bound = scope.bind(py);
            if !bound.contains("state")? {
                let namespace = TYPES.get(py)?.call_method0("SimpleNamespace")?;
                bound.set_item("state", namespace)?;
            }
            return bound
                .get_item("state")?
                .map(Bound::unbind)
                .ok_or_else(closed);
        }
        Err(closed())
    }

    #[getter]
    fn scope(&self, py: Python<'_>) -> Py<PyAny> {
        self.scope
            .as_ref()
            .map_or_else(|| py.None(), |scope| scope.clone_ref(py).into_any())
    }

    fn iter_text(self_: Py<Self>, py: Python<'_>) -> PyResult<Py<WSIterator>> {
        Py::new(py, WSIterator::new(self_, WsPayload::Text))
    }

    fn iter_json(self_: Py<Self>, py: Python<'_>) -> PyResult<Py<WSIterator>> {
        Py::new(py, WSIterator::new(self_, WsPayload::Json))
    }
}

fn closed() -> PyErr {
    PyRuntimeError::new_err("WebSocket closed")
}

/// Makes an instance of a starlette class from one keyword argument.
fn starlette(cls: &Bound<'_, PyAny>, arg_name: &str, value: &Py<PyAny>) -> PyResult<Py<PyAny>> {
    let kwargs = PyDict::new(cls.py());
    kwargs.set_item(arg_name, value.bind(cls.py()))?;
    Ok(cls.call((), Some(&kwargs))?.unbind())
}

/// Returns the raw `(bytes, bytes)` header list.
/// The list is empty until the upgrade metadata is available.
fn raw_headers(ws: &PyWebSocket, py: Python<'_>) -> Py<PyAny> {
    scope_value(ws.scope.as_ref(), py, "headers")
}

fn scope_value(scope: Option<&Py<PyDict>>, py: Python<'_>, key: &str) -> Py<PyAny> {
    scope
        .and_then(|s| s.bind(py).get_item(key).ok())
        .flatten()
        .map_or_else(|| py.None(), Bound::unbind)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WsPayload {
    Text,
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
                Ok(WSMessage::Text(bytes)) => parse_json_bytes(&bytes),
                Ok(WSMessage::Binary(_)) => Err(expected("text", "binary")),
                Ok(WSMessage::Close(_)) | Err(_) => Err(PyStopAsyncIteration::new_err(())),
            }
        })
    }
}
