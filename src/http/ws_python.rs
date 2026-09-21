use crate::runtime::py_bridge;
use bytes::{BufMut, Bytes};
use parking_lot::Mutex as PlMutex;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::utils::{json_to_py_object, write_py_json};

crate::cached_py_import!(WS_STARLETTE_HEADERS, "starlette.datastructures", "Headers");
crate::cached_py_import!(
    WS_STARLETTE_QUERY_PARAMS,
    "starlette.datastructures",
    "QueryParams"
);
crate::cached_py_import!(WS_TYPES_MODULE, "types");

pub(crate) enum WSMessage {
    Text(Bytes),
    Binary(Bytes),
    Close(u16),
}

type SharedRx = Arc<PlMutex<Option<mpsc::Receiver<WSMessage>>>>;

fn disconnected() -> PyErr {
    pyo3::exceptions::PyConnectionError::new_err("WebSocket closed")
}

fn expected(expected: &str, got: &str) -> PyErr {
    pyo3::exceptions::PyTypeError::new_err(format!("Expected {expected}, got {got}"))
}

fn parse_json_bytes(bytes: &Bytes) -> PyResult<Py<PyAny>> {
    let mut json_buf = bytes.to_vec();
    let value = simd_json::to_owned_value(&mut json_buf)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
    Ok(Python::attach(|py| json_to_py_object(py, &value)))
}

async fn next_message(rx_arc: SharedRx) -> PyResult<WSMessage> {
    let mut rx = {
        let mut guard = rx_arc.lock();
        guard.take().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(
                "Concurrent receive operations are not supported",
            )
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

#[pyclass(name = "WebSocket")]
pub struct PyWebSocket {
    pub(crate) tx: mpsc::Sender<WSMessage>,
    pub(crate) rx: SharedRx,
    pub(crate) is_connected: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) scope: Option<Py<PyDict>>,
}

#[pymethods]
impl PyWebSocket {
    fn accept<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        py_bridge::future_into_py(py, async move { Ok(()) })
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
        let mut buf = bytes::BytesMut::new();
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
                    .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string())),
                WSMessage::Binary(_) => Err(expected("text", "binary")),
                WSMessage::Close(_) => Err(disconnected()),
            }
        })
    }

    fn receive_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = self.rx.clone();
        py_bridge::future_into_py(py, async move {
            match next_message(rx).await? {
                WSMessage::Binary(data) => Ok(data.to_vec()),
                WSMessage::Text(_) => Err(expected("binary", "text")),
                WSMessage::Close(_) => Err(disconnected()),
            }
        })
    }

    fn receive_json<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = self.rx.clone();
        py_bridge::future_into_py(py, async move {
            match next_message(rx).await? {
                WSMessage::Text(bytes) => parse_json_bytes(&bytes),
                WSMessage::Binary(_) => Err(expected("text", "binary")),
                WSMessage::Close(_) => Err(disconnected()),
            }
        })
    }

    #[pyo3(signature = (code = 1000))]
    fn close<'py>(&self, py: Python<'py>, code: u16) -> PyResult<Bound<'py, PyAny>> {
        let tx = self.tx.clone();
        py_bridge::future_into_py(py, async move {
            tx.send(WSMessage::Close(code))
                .await
                .map_err(|_| closed())?;
            Ok(())
        })
    }

    #[getter]
    fn client_state(&self) -> u8 {
        if self.is_connected.load(std::sync::atomic::Ordering::Relaxed) {
            1 // Connected
        } else {
            3 // Disconnected
        }
    }

    #[getter]
    fn headers(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        starlette(WS_STARLETTE_HEADERS.get(py)?, "raw", &raw_headers(self, py))
    }

    #[getter]
    fn query_params(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        starlette(
            WS_STARLETTE_QUERY_PARAMS.get(py)?,
            "query_string",
            &scope_value(&self.scope, py, "query_string"),
        )
    }

    #[getter]
    fn state(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        if let Some(scope) = &self.scope {
            let bound = scope.bind(py);
            if !bound.contains("state")? {
                let namespace = WS_TYPES_MODULE.get(py)?.call_method0("SimpleNamespace")?;
                bound.set_item("state", namespace)?;
            }
            return bound
                .get_item("state")?
                .map(Bound::unbind)
                .ok_or_else(closed);
        }
        Err(closed())
    }

    fn iter_text(self_: Py<Self>) -> PyResult<Py<WSIterator>> {
        iterator(self_, false)
    }

    fn iter_json(self_: Py<Self>) -> PyResult<Py<WSIterator>> {
        iterator(self_, true)
    }
}

fn closed() -> PyErr {
    pyo3::exceptions::PyRuntimeError::new_err("WebSocket closed")
}

fn iterator(ws: Py<PyWebSocket>, json: bool) -> PyResult<Py<WSIterator>> {
    Python::attach(|py| Py::new(py, WSIterator { ws, json }))
}

/// Instantiates a Starlette class from a single keyword argument.
fn starlette(cls: Bound<'_, PyAny>, arg_name: &str, value: &Py<PyAny>) -> PyResult<Py<PyAny>> {
    let kwargs = PyDict::new(cls.py());
    kwargs.set_item(arg_name, value.bind(cls.py()))?;
    Ok(cls.call((), Some(&kwargs))?.unbind())
}

/// The raw `(bytes, bytes)` header list; empty until upgrade metadata lands.
fn raw_headers(ws: &PyWebSocket, py: Python<'_>) -> Py<PyAny> {
    scope_value(&ws.scope, py, "headers")
}

fn scope_value(scope: &Option<Py<PyDict>>, py: Python<'_>, key: &str) -> Py<PyAny> {
    scope
        .as_ref()
        .and_then(|s| s.bind(py).get_item(key).ok())
        .flatten()
        .map(Bound::unbind)
        .unwrap_or_else(|| py.None())
}

/// Async iterator powering `iter_text()` / `iter_json()`.
#[pyclass(name = "WSIterator")]
pub struct WSIterator {
    ws: Py<PyWebSocket>,
    json: bool,
}

#[pymethods]
impl WSIterator {
    fn __aiter__(self_: Py<Self>) -> Py<Self> {
        self_
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ws = self.ws.clone();
        let json = self.json;

        py_bridge::future_into_py(py, async move {
            let rx = Python::attach(|py| ws.borrow(py).rx.clone());
            match next_message(rx).await {
                Ok(WSMessage::Text(bytes)) if !json => {
                    let text = String::from_utf8(bytes.to_vec())
                        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
                    Ok(Python::attach(|py| {
                        text.into_pyobject(py).unwrap().into_any().unbind()
                    }))
                }
                Ok(WSMessage::Text(bytes)) => parse_json_bytes(&bytes),
                Ok(WSMessage::Binary(_)) => Err(expected("text", "binary")),
                Ok(WSMessage::Close(_)) | Err(_) => {
                    Err(pyo3::exceptions::PyStopAsyncIteration::new_err(()))
                }
            }
        })
    }
}
