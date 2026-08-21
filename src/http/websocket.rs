use crate::ffi::py_handlers::schedule_python_coroutine;
use crate::routing::dependencies::{self, DependencyExecutionError};
use crate::routing::types::PathParamRange;
use crate::runtime::executor::build_request_input_from_parts;
use axum::{extract::Extension, response::IntoResponse};
use bytes::Bytes;
use fastwebsockets::{FragmentCollector, Frame, OpCode, upgrade};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::error;

pub use crate::http::ws_python::PyWebSocket;
use crate::http::ws_python::WSMessage;

pub async fn ws_handler(
    ws: upgrade::IncomingUpgrade,
    Extension(handler): Extension<Arc<Py<PyAny>>>,
    Extension(deps): Extension<Arc<Vec<dependencies::DependencyNode>>>,
    Extension(template): Extension<Arc<str>>,
    Extension(_rt_handle): Extension<tokio::runtime::Handle>,
    Extension(async_loop): Extension<Arc<Py<PyAny>>>,
    parts: axum::http::request::Parts,
) -> axum::response::Response {
    let (response, fut) = match ws.upgrade() {
        Ok(res) => res,
        Err(_) => {
            return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    tokio::task::spawn(async move {
        if let Err(e) = handle_connection(fut, handler, deps, template, async_loop, parts).await {
            error!("WebSocket error: {e}");
        }
    });

    response.into_response()
}

async fn handle_connection(
    fut: upgrade::UpgradeFut,
    handler: Arc<Py<PyAny>>,
    deps: Arc<Vec<dependencies::DependencyNode>>,
    template: Arc<str>,
    async_loop: Arc<Py<PyAny>>,
    parts: axum::http::request::Parts,
) -> Result<(), crate::error::FastRapiError> {
    let ws_stream = fut.await?;
    let mut ws = FragmentCollector::new(ws_stream);

    let (tx_to_rust, mut rx_from_python) = mpsc::channel::<WSMessage>(1024);
    let (tx_to_python, rx_from_rust) = mpsc::channel::<WSMessage>(1024);

    let param_ranges = match_path_params(&template, parts.uri.path());
    let request_input = build_request_input_from_parts(&parts, &param_ranges);

    let (dependency_results, _teardowns) =
        dependencies::execute_dependencies(&async_loop, &deps, &request_input, None)
            .await
            .map_err(|e| match e {
                DependencyExecutionError::Python(err) => err.into(),
                DependencyExecutionError::Response(_) => crate::error::FastRapiError::from(
                    pyo3::exceptions::PyRuntimeError::new_err("websocket dependency failed"),
                ),
            })?;

    let (py_ws_obj, python_handler_future) = Python::attach(|py| {
        let py_ws_obj = Py::new(
            py,
            PyWebSocket {
                tx: tx_to_rust,
                rx: Arc::new(parking_lot::Mutex::new(Some(rx_from_rust))),
                is_connected: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                scope: Some(build_scope(py)),
            },
        )?;

        let kwargs = PyDict::new(py);
        for (name, value) in dependency_results {
            kwargs.set_item(name.as_str(), value.bind(py))?;
        }

        let coroutine = handler
            .bind(py)
            .call((py_ws_obj.clone_ref(py),), Some(&kwargs))?;

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

fn match_path_params(template: &str, actual_path: &str) -> smallvec::SmallVec<[PathParamRange; 4]> {
    let mut ranges = smallvec::SmallVec::new();

    let mut template_segments = template.split('/');
    let mut actual_segments = actual_path.split('/');

    loop {
        match (template_segments.next(), actual_segments.next()) {
            (Some(t), Some(a)) if t.starts_with('{') && t.ends_with('}') => {
                let name: Arc<str> = Arc::from(t.trim_start_matches('{').trim_end_matches('}'));
                let start = a.as_ptr() as usize - actual_path.as_ptr() as usize;
                ranges.push(PathParamRange {
                    key: name,
                    start,
                    end: start + a.len(),
                });
            }
            (Some(_), Some(_)) => {}
            _ => break,
        }
    }

    ranges
}

fn build_scope(py: Python<'_>) -> Py<PyDict> {
    let scope = PyDict::new(py);
    let _ = scope.set_item("type", "websocket");
    let _ = scope.set_item("path", "");
    let _ = scope.set_item("query_string", "");
    scope.unbind()
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
                        let code = close_code(&frame);
                        let _ = tx_to_python.send(WSMessage::Close(code)).await;
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
                    WSMessage::Close(code) => {
                        ws.write_frame(close_frame(code)).await?;
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

fn close_code(frame: &Frame<'_>) -> u16 {
    let payload: &[u8] = frame.payload.as_ref();
    payload
        .first_chunk::<2>()
        .map(|prefix| u16::from_be_bytes(*prefix))
        .unwrap_or(1000)
}

fn close_frame(code: u16) -> Frame<'static> {
    let mut payload = Vec::with_capacity(2);
    payload.extend_from_slice(&code.to_be_bytes());
    Frame::close_raw(payload.into())
}
