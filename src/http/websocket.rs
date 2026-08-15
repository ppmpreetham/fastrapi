use crate::utils::{json_to_py_object, py_any_to_json};
use axum::{extract::Extension, response::IntoResponse};
use bytes::Bytes;
use fastwebsockets::{FragmentCollector, Frame, OpCode, upgrade};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use pyo3::prelude::*;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::error;

use crate::ffi::py_handlers::schedule_python_coroutine;

pub async fn ws_handler(
    ws: upgrade::IncomingUpgrade,
    Extension(handler): Extension<Arc<Py<PyAny>>>,
    Extension(_rt_handle): Extension<tokio::runtime::Handle>,
    Extension(async_loop): Extension<Arc<Py<PyAny>>>,
) -> axum::response::Response {
    let (response, fut) = match ws.upgrade() {
        Ok(res) => res,
        Err(_) => {
            return axum::response::Response::builder()
                .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                .body(axum::body::Body::empty())
                .unwrap()
                .into_response();
        }
    };

    tokio::task::spawn(async move {
        if let Err(e) = handle_connection(fut, handler, async_loop).await {
            error!("WebSocket error: {e}");
        }
    });

    response.into_response()
}

enum WSMessage {
    Text(Bytes),
    Binary(Bytes),
    Close,
}

async fn handle_connection(
    fut: upgrade::UpgradeFut,
    handler: Arc<Py<PyAny>>,
    async_loop: Arc<Py<PyAny>>,
) -> Result<(), crate::error::FastRapiError> {
    let ws_stream = fut.await?;
    let mut ws = FragmentCollector::new(ws_stream);

    let (tx_to_rust, mut rx_from_python) = mpsc::channel::<WSMessage>(1024);
    let (tx_to_python, rx_from_rust) = mpsc::channel::<WSMessage>(1024);

    let (py_ws_obj, python_handler_future) = Python::attach(|py| {
        let py_ws_obj = Py::new(
            py,
            PyWebSocket {
                tx: tx_to_rust,
                rx: Arc::new(std::sync::Mutex::new(Some(rx_from_rust))),
                is_connected: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            },
        )?;

        let coroutine = handler.bind(py).call1((py_ws_obj.clone_ref(py),))?;
        let fut: Pin<Box<dyn Future<Output = PyResult<Py<PyAny>>> + Send>> =
            schedule_python_coroutine(py, &async_loop, coroutine)?;

        Ok::<_, PyErr>((py_ws_obj, fut))
    })?;

    tokio::select! {
        result = python_handler_future => {
            if let Err(e) = result {
                error!("Python handler error: {e}");
            }
        }

        result = socket_pump(&mut ws, tx_to_python, &mut rx_from_python) => {
            if let Err(e) = result {
                error!("Socket pump error: {e}");
            }
        }
    }

    Python::attach(|py| {
        if let Ok(py_ws) = py_ws_obj.try_borrow(py) {
            py_ws
                .is_connected
                .store(false, std::sync::atomic::Ordering::Relaxed);
        }
    });

    Ok(())
}

async fn socket_pump(
    ws: &mut FragmentCollector<TokioIo<Upgraded>>,
    tx_to_python: mpsc::Sender<WSMessage>,
    rx_from_python: &mut mpsc::Receiver<WSMessage>,
) -> Result<(), crate::error::FastRapiError> {
    loop {
        tokio::select! {
            frame = ws.read_frame() => {
                let frame = frame?;
                match frame.opcode {
                    OpCode::Close => {
                        let _ = tx_to_python.send(WSMessage::Close).await;
                        break;
                    }
                    OpCode::Text | OpCode::Binary => {
                        let bytes = Bytes::from(frame.payload.to_vec());
                        let msg = if frame.opcode == OpCode::Text {
                            WSMessage::Text(bytes)
                        } else {
                            WSMessage::Binary(bytes)
                        };
                        if tx_to_python.send(msg).await.is_err() {
                            break;
                        }
                    }
                    OpCode::Ping => {
                        let payload = frame.payload.to_vec();
                        ws.write_frame(Frame::pong(payload.into())).await?;
                    }
                    OpCode::Pong => {}
                    _ => {}
                }
            }

            Some(msg) = rx_from_python.recv() => {
                match msg {
                    WSMessage::Text(bytes) => {
                        ws.write_frame(Frame::text(bytes.to_vec().into())).await?;
                    }
                    WSMessage::Binary(data) => {
                        ws.write_frame(Frame::binary(data.to_vec().into())).await?;
                    }
                    WSMessage::Close => {
                        ws.write_frame(Frame::close_raw(vec![].into())).await?;
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

#[pyclass(name = "WebSocket")]
pub struct PyWebSocket {
    tx: mpsc::Sender<WSMessage>,
    rx: Arc<std::sync::Mutex<Option<mpsc::Receiver<WSMessage>>>>,
    is_connected: Arc<std::sync::atomic::AtomicBool>,
}

async fn fetch_msg(
    rx_arc: Arc<std::sync::Mutex<Option<mpsc::Receiver<WSMessage>>>>,
) -> PyResult<WSMessage> {
    let mut rx = rx_arc
        .lock()
        .map_err(|_| pyo3::exceptions::PyRuntimeError::new_err("Mutex Poisoned"))?
        .take()
        .ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(
                "Concurrent receive operations are not supported",
            )
        })?;

    let res = rx.recv().await;
    if let Ok(mut lock) = rx_arc.lock() {
        *lock = Some(rx);
    } else {
        return Err(pyo3::exceptions::PyRuntimeError::new_err("Mutex Poisoned"));
    }

    match res {
        Some(WSMessage::Close) | None => Err(pyo3::exceptions::PyConnectionError::new_err(
            "WebSocket closed",
        )),
        Some(msg) => Ok(msg),
    }
}

#[pymethods]
impl PyWebSocket {
    fn accept<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        rsloop::rust_async::future_into_py(py, async move { Ok(()) })
    }

    fn send_text<'py>(&self, py: Python<'py>, data: String) -> PyResult<Bound<'py, PyAny>> {
        let tx = self.tx.clone();
        let bytes = Bytes::from(data);

        rsloop::rust_async::future_into_py(py, async move {
            tx.send(WSMessage::Text(bytes))
                .await
                .map_err(|_| pyo3::exceptions::PyRuntimeError::new_err("WebSocket closed"))?;
            Ok(())
        })
    }

    fn send_bytes<'py>(&self, py: Python<'py>, data: Vec<u8>) -> PyResult<Bound<'py, PyAny>> {
        let tx = self.tx.clone();
        let bytes = Bytes::from(data);

        rsloop::rust_async::future_into_py(py, async move {
            tx.send(WSMessage::Binary(bytes))
                .await
                .map_err(|_| pyo3::exceptions::PyRuntimeError::new_err("WebSocket closed"))?;
            Ok(())
        })
    }

    fn send_json<'py>(
        &self,
        py: Python<'py>,
        data: &Bound<'_, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let json_str = py_any_to_json(py, data).to_string();
        self.send_text(py, json_str)
    }

    fn receive_text<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx_arc = self.rx.clone();

        rsloop::rust_async::future_into_py(py, async move {
            match fetch_msg(rx_arc).await? {
                WSMessage::Text(bytes) => String::from_utf8(bytes.to_vec())
                    .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string())),
                WSMessage::Binary(_) => Err(pyo3::exceptions::PyTypeError::new_err(
                    "Expected text, got binary",
                )),
                WSMessage::Close => unreachable!(),
            }
        })
    }

    fn receive_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx_arc = self.rx.clone();

        rsloop::rust_async::future_into_py(py, async move {
            match fetch_msg(rx_arc).await? {
                WSMessage::Binary(data) => Ok(data.to_vec()),
                WSMessage::Text(_) => Err(pyo3::exceptions::PyTypeError::new_err(
                    "Expected binary, got text",
                )),
                WSMessage::Close => unreachable!(),
            }
        })
    }

    fn receive_json<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx_arc = self.rx.clone();

        rsloop::rust_async::future_into_py(py, async move {
            match fetch_msg(rx_arc).await? {
                WSMessage::Text(bytes) => {
                    let text = String::from_utf8(bytes.to_vec())
                        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;

                    Python::attach(|py| {
                        let json: sonic_rs::Value = sonic_rs::from_str(&text)
                            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
                        Ok(json_to_py_object(py, &json))
                    })
                }
                WSMessage::Binary(_) => Err(pyo3::exceptions::PyTypeError::new_err(
                    "Expected text, got binary",
                )),
                WSMessage::Close => unreachable!(),
            }
        })
    }

    fn close<'py>(&self, py: Python<'py>, _code: Option<u16>) -> PyResult<Bound<'py, PyAny>> {
        let tx = self.tx.clone();

        rsloop::rust_async::future_into_py(py, async move {
            tx.send(WSMessage::Close).await.map_err(|_| {
                pyo3::exceptions::PyRuntimeError::new_err("WebSocket already closed")
            })?;
            Ok(())
        })
    }

    #[getter]
    fn client_state(&self) -> PyResult<u8> {
        if self.is_connected.load(std::sync::atomic::Ordering::Relaxed) {
            Ok(1) // Connected
        } else {
            Ok(3) // Disconnected
        }
    }
}
