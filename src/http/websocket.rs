use crate::routing::dependencies::{self, DependencyExecutionError};
use crate::routing::types::PathParamRange;
use crate::runtime::executor::{build_request_input_from_parts, schedule_python_coroutine};
use crate::runtime::py_bridge;
use axum::{extract::State, response::IntoResponse};
use bytes::Bytes;
use fastwebsockets::{FragmentCollector, Frame, OpCode, Payload, upgrade};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, timeout};
use tracing::error;

pub use crate::http::ws_python::PyWebSocket;
use crate::http::ws_python::WSMessage;
use axum::extract::ConnectInfo;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::Response;
use crate::error::FastRapiError;
use crate::globals::serve_app;
use crate::globals::spawn;
use crate::http::ws_python::WSDecision;
use crate::routing::types::RequestInput;
use crate::runtime::executor::execute_teardowns_async;
use parking_lot::Mutex;
use pyo3::types::PyList;
use smallvec::SmallVec;
use std::borrow::Cow;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;

#[derive(Clone)]
pub struct WsRouteState {
    pub handler: Arc<Py<PyAny>>,
    pub deps: Arc<Vec<dependencies::DependencyNode>>,
    pub template: Arc<str>,
    pub param_names: Arc<[Arc<str>]>,
    pub async_loop: Arc<Py<PyAny>>,
    pub dependency_overrides: Option<Py<PyAny>>,
}

pub async fn ws_handler(
    ws: upgrade::IncomingUpgrade,
    State(route): State<Arc<WsRouteState>>,
    parts: Parts,
) -> Response {
    let (tx_to_rust, rx_from_python) = mpsc::channel::<WSMessage>(1024);
    let (tx_to_python, rx_from_rust) = mpsc::channel::<WSMessage>(1024);
    let (decision_tx, mut decision_rx) = oneshot::channel();

    let param_ranges = match_path_params(&route, parts.uri.path());
    let request_input = build_request_input_from_parts(&parts, &param_ranges, &route.param_names);

    let plan = match Python::attach(|py| {
        dependencies::request_plan(
            py,
            &route.deps,
            route.dependency_overrides.as_ref(),
            &route.param_names,
        )
        .map(Cow::into_owned)
    }) {
        Ok(plan) => plan,
        Err(error) => {
            error!("WebSocket dependency plan error: {error}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let (dependency_results, teardowns) =
        match dependencies::execute_dependencies(&route.async_loop, &plan, &request_input, None)
            .await
        {
            Ok(result) => result,
            Err(DependencyExecutionError::Python(error)) => {
                error!("WebSocket dependency error: {error}");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            Err(DependencyExecutionError::Response(_)) => {
                error!("WebSocket dependency returned a response");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };

    let (py_ws_obj, mut python_handler_future) = match Python::attach(|py| {
        let py_ws_obj = Py::new(
            py,
            PyWebSocket {
                tx: tx_to_rust,
                rx: Arc::new(Mutex::new(Some(rx_from_rust))),
                is_connected: Arc::new(AtomicBool::new(true)),
                scope: Some(build_scope(py, &parts, &request_input)),
                decision: Arc::new(Mutex::new(Some(decision_tx))),
                handshake_state: Arc::new(AtomicU8::new(0)),
            },
        )?;

        let kwargs = PyDict::new(py);
        for (name, value) in dependency_results {
            kwargs.set_item(name, value.bind(py))?;
        }
        let parameters = py
            .import("inspect")?
            .call_method1("signature", (route.handler.bind(py),))?
            .getattr("parameters")?;
        for name in route.param_names.iter() {
            if parameters.contains(name.as_ref())?
                && let Some(value) = request_input.get_path_param(name)
            {
                kwargs.set_item(name.as_ref(), value)?;
            }
        }

        let coroutine = route
            .handler
            .bind(py)
            .call((py_ws_obj.clone_ref(py),), Some(&kwargs))?;

        let fut: py_bridge::PyTaskFuture =
            schedule_python_coroutine(py, &route.async_loop, coroutine)?;

        Ok::<_, PyErr>((py_ws_obj, fut))
    }) {
        Ok(value) => value,
        Err(_error) => {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let decision = timeout(Duration::from_secs(10), async {
        tokio::select! {
            decision = &mut decision_rx => decision.ok(),
            result = &mut python_handler_future => {
                if let Err(error) = result {
                    error!("Python websocket handler error: {error}");
                } else {
                    error!("Python websocket handler returned before handshake");
                }
                None
            }
        }
    })
    .await;
    let decision = if let Ok(Some(decision)) = decision { decision } else {
        execute_teardowns_async(&route.async_loop, teardowns).await;
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    match decision {
        WSDecision::Reject(code) => {
            let _ = code;
            execute_teardowns_async(&route.async_loop, teardowns).await;
            StatusCode::FORBIDDEN.into_response()
        }
        WSDecision::Accept => {
            let (response, fut) = match ws.upgrade() {
                Ok(result) => result,
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            };
            let connection_route = route.clone();
            spawn(async move {
                if let Err(error) = handle_connection(
                    fut,
                    py_ws_obj,
                    python_handler_future,
                    teardowns,
                    connection_route,
                    tx_to_python,
                    rx_from_python,
                )
                .await
                {
                    error!("WebSocket error: {error}");
                }
            });
            response.into_response()
        }
    }
}

async fn handle_connection(
    fut: upgrade::UpgradeFut,
    py_ws_obj: Py<PyWebSocket>,
    python_handler_future: py_bridge::PyTaskFuture,
    teardowns: dependencies::TeardownTasks,
    route: Arc<WsRouteState>,
    tx_to_python: mpsc::Sender<WSMessage>,
    mut rx_from_python: mpsc::Receiver<WSMessage>,
) -> Result<(), FastRapiError> {
    let ws_stream = fut.await?;
    let mut ws = FragmentCollector::new(ws_stream);
    tokio::select! {
        result = python_handler_future => {
            if let Err(error) = result {
                error!("Python handler error: {error}");
            }
        }
        result = socket_pump(&mut ws, tx_to_python, &mut rx_from_python) => {
            if let Err(error) = result {
                error!("Socket pump error: {error}");
            }
        }
    }
    Python::attach(|py| {
        if let Ok(websocket) = py_ws_obj.try_borrow(py) {
            websocket
                .is_connected
                .store(false, Ordering::Release);
        }
    });
    execute_teardowns_async(&route.async_loop, teardowns).await;
    Ok(())
}

#[must_use]
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
    let mut ranges = SmallVec::new();

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

fn build_scope(
    py: Python<'_>,
    parts: &Parts,
    input: &RequestInput<'_>,
) -> Py<PyDict> {
    let scope = PyDict::new(py);
    _ = scope.set_item("type", "websocket");
    _ = scope.set_item("path", parts.uri.path());
    _ = scope.set_item(
        "query_string",
        parts.uri.query().unwrap_or_default().as_bytes(),
    );
    let headers = PyList::empty(py);
    for (name, value) in &parts.headers {
        _ = headers.append((name.as_str().as_bytes(), value.as_bytes()));
    }
    _ = scope.set_item("headers", headers);
    let path_params = PyDict::new(py);
    if let Some(params) = input.path_params.get() {
        for (name, value) in params {
            _ = path_params.set_item(*name, *value);
        }
    }
    _ = scope.set_item("path_params", path_params);
    if let Some(app) = serve_app() {
        _ = scope.set_item("app", app.bind(py));
    } else {
        _ = scope.set_item("app", py.None());
    }
    if let Some(connect) = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
    {
        _ = scope.set_item("client", (connect.0.ip().to_string(), connect.0.port()));
    } else {
        _ = scope.set_item("client", py.None());
    }
    scope.unbind()
}

async fn socket_pump(
    ws: &mut FragmentCollector<TokioIo<Upgraded>>,
    tx_to_python: mpsc::Sender<WSMessage>,
    rx_from_python: &mut mpsc::Receiver<WSMessage>,
) -> Result<(), FastRapiError> {
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
        .map_or(1000, |prefix| u16::from_be_bytes(*prefix))
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
