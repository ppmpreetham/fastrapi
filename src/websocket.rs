use axum::{extract::Extension, response::IntoResponse};
use bytes::Bytes;
use fastwebsockets::{upgrade, FragmentCollector, Frame, OpCode};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use once_cell::sync::Lazy;
use papaya::HashMap;
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyTuple};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::error;

pub static WEBSOCKET_ROUTES: Lazy<HashMap<String, Py<PyAny>>> =
    Lazy::new(|| HashMap::with_capacity(32));

/// @app.websocket("/ws") decorator
#[pyfunction]
pub fn websocket(path: String) -> PyResult<Py<PyAny>> {
    let route_key = format!("WS {path}");

    Python::attach(|py| {
        let closure = move |args: &Bound<'_, PyTuple>,
                            _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> {
            let func = args.get_item(0)?;
            let stored_func = func.clone().unbind();

            WEBSOCKET_ROUTES
                .pin()
                .insert(route_key.clone(), stored_func);

            Ok(func.unbind())
        };

        let py_func = PyCFunction::new_closure(py, None, None, closure)?;
        Ok(py_func.into_any().unbind())
    })
}

pub async fn ws_handler(
    ws: upgrade::IncomingUpgrade,
    Extension(route_key): Extension<Arc<String>>,
    Extension(_rt_handle): Extension<tokio::runtime::Handle>,
) -> impl IntoResponse {
    let (response, fut) = ws.upgrade().expect("WebSocket upgrade failed");

    tokio::task::spawn(async move {
        if let Err(e) = handle_connection(fut, route_key).await {
            error!("WebSocket error: {e}");
        }
    });

    response
}

enum WSMessage {
    Text(Bytes),
    Binary(Bytes),
    Close,
}

async fn handle_connection(
    fut: upgrade::UpgradeFut,
    route_key: Arc<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ws_stream = fut.await?;
    let mut ws = FragmentCollector::new(ws_stream);
    let handler: Py<PyAny> = {
        let guard = crate::utils::local_guard(&*WEBSOCKET_ROUTES);
        WEBSOCKET_ROUTES
            .get(&*route_key, &guard)
            .cloned()
            .ok_or("Route not found")?
    };

    let (tx_to_rust, mut rx_from_python) = mpsc::channel::<WSMessage>(1024);
    let (tx_to_python, rx_from_rust) = mpsc::channel::<WSMessage>(1024);

    let py_ws_obj: Py<PyWebSocket> = Python::attach(|py| {
        Py::new(
            py,
            PyWebSocket {
                tx: tx_to_rust,
                rx: Arc::new(tokio::sync::Mutex::new(rx_from_rust)),
                is_connected: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            },
        )
    })?;

    let python_handler_future = Python::attach(|py| {
        let coroutine = handler.call1(py, (py_ws_obj.clone(),))?;
        let bound = coroutine.bind(py);
        pyo3_async_runtimes::tokio::into_future(bound.clone())
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
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
    rx: Arc<tokio::sync::Mutex<mpsc::Receiver<WSMessage>>>,
    is_connected: Arc<std::sync::atomic::AtomicBool>,
}

#[pymethods]
impl PyWebSocket {
    fn accept(&self) -> PyResult<()> {
        Ok(())
    }

    fn send_text<'py>(&self, py: Python<'py>, data: String) -> PyResult<Bound<'py, PyAny>> {
        let tx = self.tx.clone();
        let bytes = Bytes::from(data);

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            tx.send(WSMessage::Text(bytes))
                .await
                .map_err(|_| pyo3::exceptions::PyRuntimeError::new_err("WebSocket closed"))?;
            Ok(())
        })
    }

    fn send_bytes<'py>(&self, py: Python<'py>, data: Vec<u8>) -> PyResult<Bound<'py, PyAny>> {
        let tx = self.tx.clone();
        let bytes = Bytes::from(data);

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
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
        let json_str = crate::utils::py_any_to_json(py, data).to_string();
        self.send_text(py, json_str)
    }

    fn receive_text<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = self.rx.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut rx_guard = rx.lock().await;
            match rx_guard.recv().await {
                Some(WSMessage::Text(bytes)) => String::from_utf8(bytes.to_vec())
                    .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string())),
                Some(WSMessage::Close) | None => Err(pyo3::exceptions::PyConnectionError::new_err(
                    "WebSocket closed",
                )),
                Some(WSMessage::Binary(_)) => Err(pyo3::exceptions::PyTypeError::new_err(
                    "Expected text, got binary",
                )),
            }
        })
    }

    fn receive_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = self.rx.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut rx_guard = rx.lock().await;
            match rx_guard.recv().await {
                Some(WSMessage::Binary(data)) => Ok(data.to_vec()),
                Some(WSMessage::Close) | None => Err(pyo3::exceptions::PyConnectionError::new_err(
                    "WebSocket closed",
                )),
                Some(WSMessage::Text(_)) => Err(pyo3::exceptions::PyTypeError::new_err(
                    "Expected binary, got text",
                )),
            }
        })
    }

    fn receive_json<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = self.rx.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut rx_guard = rx.lock().await;
            match rx_guard.recv().await {
                Some(WSMessage::Text(bytes)) => {
                    let text = String::from_utf8(bytes.to_vec())
                        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;

                    Python::attach(|py| {
                        let json: serde_json::Value = serde_json::from_str(&text)
                            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
                        Ok(crate::utils::json_to_py_object(py, &json))
                    })
                }
                Some(WSMessage::Close) | None => Err(pyo3::exceptions::PyConnectionError::new_err(
                    "WebSocket closed",
                )),
                Some(WSMessage::Binary(_)) => Err(pyo3::exceptions::PyTypeError::new_err(
                    "Expected text, got binary",
                )),
            }
        })
    }

    fn close<'py>(&self, py: Python<'py>, _code: Option<u16>) -> PyResult<Bound<'py, PyAny>> {
        let tx = self.tx.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
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

pub fn register_websocket_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(websocket, m)?)?;
    m.add_class::<PyWebSocket>()?;
    Ok(())
}

pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();

    let websocket_module = PyModule::new(py, "websocket")?;
    register_websocket_module(&websocket_module)?;
    parent.add_submodule(&websocket_module)?;
    let sys_modules = py.import("sys")?.getattr("modules")?;
    sys_modules.set_item("fastrapi.websocket", &websocket_module)?;

    Ok(())
}
