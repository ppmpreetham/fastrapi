use crate::runtime::py_bridge;
use crate::routing::dependencies::{self, DependencyExecutionError};
use crate::routing::types::PathParamRange;
use crate::runtime::executor::{build_request_input_from_parts, schedule_python_coroutine};
use axum::{extract::State, response::IntoResponse};
use bytes::Bytes;
use fastwebsockets::{FragmentCollector, Frame, OpCode, Payload, upgrade};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::error;

pub use crate::http::ws_python::PyWebSocket;
use crate::http::ws_python::WSMessage;

#[derive(Clone)]
pub struct WsRouteState {
    pub handler: Arc<Py<PyAny>>,
    pub deps: Arc<Vec<dependencies::DependencyNode>>,
    pub template: Arc<str>,
    pub param_names: Arc<[Arc<str>]>,
    pub async_loop: Arc<Py<PyAny>>,
}

pub async fn ws_handler(
    ws: upgrade::IncomingUpgrade,
    State(route): State<Arc<WsRouteState>>,
    parts: axum::http::request::Parts,
) -> axum::response::Response {
    let (response, fut) = match ws.upgrade() {
        Ok(res) => res,
        Err(_) => {
            return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    crate::globals::spawn(async move {
        if let Err(e) = handle_connection(fut, route, parts).await {
            error!("WebSocket error: {e}");
        }
    });

    response.into_response()
}

async fn handle_connection(
    fut: upgrade::UpgradeFut,
    route: Arc<WsRouteState>,
    parts: axum::http::request::Parts,
) -> Result<(), crate::error::FastRapiError> {
    let ws_stream = fut.await?;
    let mut ws = FragmentCollector::new(ws_stream);

    let (tx_to_rust, mut rx_from_python) = mpsc::channel::<WSMessage>(1024);
    let (tx_to_python, rx_from_rust) = mpsc::channel::<WSMessage>(1024);

    let param_ranges = match_path_params(&route, parts.uri.path());
    let request_input = build_request_input_from_parts(&parts, &param_ranges, &route.param_names);

    let (dependency_results, _teardowns) =
        dependencies::execute_dependencies(&route.async_loop, &route.deps, &request_input, None)
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
            kwargs.set_item(name, value.bind(py))?;
        }

        let coroutine = route
            .handler
            .bind(py)
            .call((py_ws_obj.clone_ref(py),), Some(&kwargs))?;

        let fut: py_bridge::PyTaskFuture =
            schedule_python_coroutine(py, &route.async_loop, coroutine)?;

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

pub fn ws_param_names(template: &str) -> Arc<[Arc<str>]> {
    template
        .split('/')
        .filter(|s| s.starts_with('{') && s.ends_with('}'))
        .map(|s| {
            let name = s.trim_start_matches('{').trim_end_matches('}');
            Arc::from(name.split(':').next().unwrap_or(name))
        })
        .collect()
}

fn match_path_params(
    route: &WsRouteState,
    actual_path: &str,
) -> smallvec::SmallVec<[PathParamRange; 4]> {
    let mut ranges = smallvec::SmallVec::new();

    let mut template_segments = route.template.split('/');
    let mut actual_segments = actual_path.split('/');
    let mut name_index = 0;

    loop {
        match (template_segments.next(), actual_segments.next()) {
            (Some(t), Some(a)) if t.starts_with('{') && t.ends_with('}') => {
                let start = a.as_ptr() as usize - actual_path.as_ptr() as usize;
                ranges.push(PathParamRange {
                    name_index,
                    start,
                    end: start + a.len(),
                });
                name_index += 1;
            }
            (Some(_), Some(_)) => {}
            _ => break,
        }
    }

    ranges
}

fn build_scope(py: Python<'_>) -> Py<PyDict> {
    let scope = PyDict::new(py);
    _ = scope.set_item("type", "websocket");
    _ = scope.set_item("path", "");
    _ = scope.set_item("query_string", "");
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
                        _ = tx_to_python.send(WSMessage::Close(code)).await;
                        break;
                    }
                    OpCode::Text | OpCode::Binary => {
                        let bytes = payload_into_bytes(frame.payload);
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
                        ws.write_frame(Frame::pong(payload_into_owned(frame.payload)))
                            .await?;
                    }
                    OpCode::Pong => {}
                    _ => {}
                }
            }

            Some(msg) = rx_from_python.recv() => {
                match msg {
                    WSMessage::Text(bytes) => {
                        ws.write_frame(Frame::text(bytes_into_payload(bytes))).await?;
                    }
                    WSMessage::Binary(data) => {
                        ws.write_frame(Frame::binary(bytes_into_payload(data)))
                            .await?;
                    }
                    WSMessage::Close(code) => {
                        ws.write_frame(Frame::close(code, &[])).await?;
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

fn payload_into_bytes(payload: Payload<'_>) -> Bytes {
    match payload {
        Payload::Owned(vec) => Bytes::from(vec),
        Payload::Bytes(buf) => buf.freeze(),
        Payload::Borrowed(slice) => Bytes::copy_from_slice(slice),
        Payload::BorrowedMut(slice) => Bytes::copy_from_slice(slice),
    }
}

fn payload_into_owned(payload: Payload<'_>) -> Payload<'static> {
    match payload {
        Payload::Owned(vec) => Payload::Owned(vec),
        Payload::Bytes(buf) => Payload::Bytes(buf),
        Payload::Borrowed(slice) => Payload::Owned(slice.to_vec()),
        Payload::BorrowedMut(slice) => Payload::Owned(slice.to_vec()),
    }
}

fn bytes_into_payload(bytes: Bytes) -> Payload<'static> {
    Payload::Bytes(bytes.into())
}
