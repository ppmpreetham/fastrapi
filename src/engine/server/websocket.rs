//! The WebSocket support of the server.
//! It accepts WebSocket connections, matches the path parameters and moves messages between the socket and the Python handler.

use crate::engine::executor::{build_request_input_from_parts, schedule_python_coroutine};
use axum::extract::{ConnectInfo, State};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use fastrapi_core::params::PathParamRange;
use fastrapi_routing::dependencies::{self, DependencyExecutionError, execute_teardowns_async};
use fastrapi_routing::request_data::{ValidationErrorDetails, resolve_parameter_value};
use fastrapi_routing::route::ParsedParameter;
use fastrapi_runtime::py_bridge;
use fastrapi_runtime::py_bridge::EventLoop;
use fastwebsockets::{
    FragmentCollectorRead, Frame, OpCode, Payload, WebSocketError, WebSocketWrite, upgrade,
};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{IntoPyDict, PyDict, PyList, PyString};
use simd_json::OwnedValue as JsonValue;
use simd_json::prelude::{ValueAsArray, ValueAsObject};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{ReadHalf, WriteHalf};
use tokio::sync::{mpsc, oneshot};
use tracing::error;

#[derive(Debug, thiserror::Error)]
enum SocketPumpError {
    #[error("WebSocket Error: {0}")]
    WebSocket(#[from] fastwebsockets::WebSocketError),
    #[error("Python Error: {0}")]
    Python(#[from] PyErr),
}

use crate::engine::exception_dispatch::find_handler;
use axum::http::StatusCode;
use axum::http::header::HOST;
use axum::http::request::Parts;
use fastrapi_core::path::host_without_port;
use fastrapi_http::exceptions::{
    PyWebSocketDisconnect, PyWebSocketException, PyWebSocketRequestValidationError,
};
use fastrapi_http::request::PyHTTPConnection;
use fastrapi_http::request_input::RequestInput;
use fastrapi_http::websocket::PyWebSocket;
use fastrapi_http::websocket::{WSDecision, WSMessage};
use fastrapi_pyutil::json::{json_to_py_object, py_any_to_json};
use fastrapi_runtime::tokio_runtime::spawn;
use parking_lot::Mutex;
use smallvec::SmallVec;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

#[derive(Clone)]
pub struct WsRouteState {
    pub handler: Arc<Py<PyAny>>,
    pub websocket_arg: Option<Py<PyString>>,
    pub app: Py<PyAny>,
    pub params: Arc<[ParsedParameter]>,
    pub deps: Arc<Vec<dependencies::DependencyNode>>,
    pub param_segments: Arc<[usize]>,
    pub segment_count: usize,
    pub param_names: Arc<[Arc<str>]>,
    pub event_loop: EventLoop,
    pub dependency_overrides: Option<Py<PyAny>>,
    pub override_cache: Arc<dependencies::OverrideCache>,
    pub host: Option<Box<str>>,
}

pub async fn ws_host_handler(
    ws: upgrade::IncomingUpgrade,
    State(routes): State<Arc<[Arc<WsRouteState>]>>,
    parts: Parts,
) -> Response {
    let host = parts
        .headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .map(host_without_port);
    let route = routes.iter().find(|route| {
        route
            .host
            .as_deref()
            .is_none_or(|expected| host == Some(expected))
    });
    match route {
        Some(route) => ws_handler(ws, State(route.clone()), parts).await,
        None => StatusCode::NOT_FOUND.into_response(),
    }
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
    let request_input =
        build_request_input_from_parts(&parts, &param_ranges, &route.param_names, route.event_loop);

    let plan = match Python::attach(|py| {
        dependencies::request_plan(
            py,
            &route.deps,
            route.dependency_overrides.as_ref(),
            &route.param_names,
            &route.override_cache,
        )
    }) {
        Ok(plan) => plan,
        Err(error) => {
            error!("WebSocket dependency plan error: {error}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let py_ws_obj = match Python::attach(|py| {
        let connection = PyHTTPConnection {
            scope: build_scope(py, &parts, &request_input, route.app.bind(py)).into_any(),
            receive: py.None(),
        };
        Py::new(
            py,
            PyClassInitializer::from(connection).add_subclass(PyWebSocket {
                tx: tx_to_rust,
                rx: Arc::new(Mutex::new(Some(rx_from_rust))),
                is_connected: Arc::new(AtomicBool::new(true)),
                decision: Arc::new(Mutex::new(Some(decision_tx))),
                handshake_state: Arc::new(AtomicU8::new(0)),
                connect_delivered: Arc::new(AtomicBool::new(false)),
                denial: Arc::new(Mutex::new(None)),
            }),
        )
    }) {
        Ok(websocket) => websocket,
        Err(error) => {
            error!("WebSocket setup error: {error}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let websocket_arg = Python::attach(|py| py_ws_obj.clone_ref(py).into_any());

    let (dependency_results, teardowns) = match dependencies::execute_dependencies(
        route.event_loop,
        plan.nodes(),
        &request_input,
        Some(websocket_arg),
    )
    .await
    {
        Ok(result) => result,
        Err(DependencyExecutionError::Python(error)) => {
            return rejected(&route, &py_ws_obj, error, &mut decision_rx, None).await;
        }
        Err(DependencyExecutionError::Response(response)) => {
            let errors = validation_errors(&response);
            if errors.is_empty() {
                error!("WebSocket dependency returned a response");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            let error = Python::attach(|py| validation_exception(py, &errors));
            return rejected(&route, &py_ws_obj, error, &mut decision_rx, None).await;
        }
    };

    let mut python_handler_future = match Python::attach(|py| {
        let kwargs = PyDict::new(py);
        for (name, value) in dependency_results {
            kwargs.set_item(name, value.bind(py))?;
        }
        let mut errors = Vec::new();
        for param in route.params.iter() {
            match resolve_parameter_value(py, param, &request_input) {
                Ok(Some(value)) => kwargs.set_item(param.name_py.bind(py), value)?,
                Ok(None) => {}
                Err(response) => errors.extend(validation_errors(&response)),
            }
        }
        if !errors.is_empty() {
            return Ok(Err(errors));
        }

        let coroutine = if let Some(name) = &route.websocket_arg {
            kwargs.set_item(name.bind(py), py_ws_obj.clone_ref(py))?;
            route.handler.bind(py).call((), Some(&kwargs))?
        } else {
            route
                .handler
                .bind(py)
                .call((py_ws_obj.clone_ref(py),), Some(&kwargs))?
        };

        let fut: py_bridge::PyTaskFuture =
            schedule_python_coroutine(py, route.event_loop, coroutine)?;

        Ok::<_, PyErr>(Ok(fut))
    }) {
        Ok(Ok(value)) => value,
        Ok(Err(errors)) => {
            let error = Python::attach(|py| validation_exception(py, &errors));
            return rejected(&route, &py_ws_obj, error, &mut decision_rx, Some(teardowns)).await;
        }
        Err(error) => {
            return rejected(&route, &py_ws_obj, error, &mut decision_rx, Some(teardowns)).await;
        }
    };

    let handshake = tokio::select! {
        decision = &mut decision_rx => Handshake::Decision(decision.ok()),
        result = &mut python_handler_future => Handshake::Finished(result),
    };
    let (decision, handler_run) = match handshake {
        Handshake::Decision(Some(decision)) => {
            (decision, HandlerRun::Running(python_handler_future))
        }
        Handshake::Decision(None) => {
            execute_teardowns_async(route.event_loop, teardowns).await;
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        Handshake::Finished(Err(error)) => {
            return rejected(&route, &py_ws_obj, error, &mut decision_rx, Some(teardowns)).await;
        }
        Handshake::Finished(Ok(value)) => {
            let Ok(decision) = decision_rx.try_recv() else {
                error!("Python websocket handler returned before handshake");
                execute_teardowns_async(route.event_loop, teardowns).await;
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            };
            (decision, HandlerRun::Finished(Ok(value)))
        }
    };

    match decision {
        WSDecision::Reject(_) => {
            execute_teardowns_async(route.event_loop, teardowns).await;
            StatusCode::FORBIDDEN.into_response()
        }
        WSDecision::Deny(response) => {
            execute_teardowns_async(route.event_loop, teardowns).await;
            response
        }
        WSDecision::Accept => {
            let Ok((response, fut)) = ws.upgrade() else {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            };
            let connection_route = route.clone();
            spawn(async move {
                if let Err(error) = handle_connection(
                    fut,
                    py_ws_obj,
                    handler_run,
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

enum Handshake {
    Decision(Option<WSDecision>),
    Finished(PyResult<Py<PyAny>>),
}

enum HandlerRun {
    Running(py_bridge::PyTaskFuture),
    Finished(PyResult<Py<PyAny>>),
}

enum HandlerOutcome {
    Disconnected,
    Handled,
    Close(u16, String),
    Unhandled,
}

fastrapi_pyutil::cached_py_import!(
    STARLETTE_WEBSOCKET_EXCEPTION,
    "starlette.exceptions",
    "WebSocketException"
);

fn validation_exception(py: Python<'_>, errors: &[JsonValue]) -> PyErr {
    let list = PyList::empty(py);
    for error in errors {
        _ = list.append(json_to_py_object(py, error));
    }
    match py
        .get_type::<PyWebSocketRequestValidationError>()
        .call1((list,))
    {
        Ok(value) => PyErr::from_value(value),
        Err(error) => error,
    }
}

fn websocket_exception_close(py: Python<'_>, value: &Bound<'_, PyAny>) -> Option<(u16, String)> {
    let is_websocket_exception = value.is_instance_of::<PyWebSocketException>()
        || STARLETTE_WEBSOCKET_EXCEPTION
            .get(py)
            .is_ok_and(|class| value.is_instance(class).unwrap_or(false));
    if !is_websocket_exception {
        return None;
    }
    let code = value.getattr("code").ok()?.extract::<u16>().ok()?;
    let reason = value
        .getattr("reason")
        .ok()
        .and_then(|reason| reason.extract::<Option<String>>().ok())
        .flatten()
        .unwrap_or_default();
    Some((code, reason))
}

async fn handle_error(
    route: &WsRouteState,
    websocket: &Py<PyWebSocket>,
    error: PyErr,
) -> HandlerOutcome {
    let pending = Python::attach(|py| -> Result<py_bridge::PyTaskFuture, HandlerOutcome> {
        let value = error.value(py);
        if value.is_instance_of::<PyWebSocketDisconnect>() {
            return Err(HandlerOutcome::Disconnected);
        }
        let handler = route
            .event_loop
            .server()
            .exception_handlers()
            .and_then(|registry| find_handler(py, registry.bind(py), &error));
        let Some(handler) = handler else {
            if let Some((code, reason)) = websocket_exception_close(py, value) {
                return Err(HandlerOutcome::Close(code, reason));
            }
            if value.is_instance_of::<PyWebSocketRequestValidationError>() {
                let reason = value
                    .call_method0("errors")
                    .ok()
                    .and_then(|errors| simd_json::to_string(&py_any_to_json(py, &errors)).ok())
                    .unwrap_or_default();
                return Err(HandlerOutcome::Close(1008, reason));
            }
            error.print(py);
            return Err(HandlerOutcome::Unhandled);
        };
        match handler.call1((websocket.clone_ref(py), value)) {
            Ok(result) if result.hasattr("__await__").unwrap_or(false) => {
                schedule_python_coroutine(py, route.event_loop, result).map_err(|error| {
                    error.print(py);
                    HandlerOutcome::Unhandled
                })
            }
            Ok(_) => Err(HandlerOutcome::Handled),
            Err(handler_error) => {
                handler_error.print(py);
                Err(HandlerOutcome::Unhandled)
            }
        }
    });
    match pending {
        Ok(future) => match future.await {
            Ok(_) => HandlerOutcome::Handled,
            Err(error) => {
                Python::attach(|py| error.print(py));
                HandlerOutcome::Unhandled
            }
        },
        Err(outcome) => outcome,
    }
}

async fn rejected(
    route: &WsRouteState,
    websocket: &Py<PyWebSocket>,
    error: PyErr,
    decision_rx: &mut oneshot::Receiver<WSDecision>,
    teardowns: Option<dependencies::TeardownTasks>,
) -> Response {
    let outcome = handle_error(route, websocket, error).await;
    if let Some(teardowns) = teardowns {
        execute_teardowns_async(route.event_loop, teardowns).await;
    }
    match decision_rx.try_recv() {
        Ok(WSDecision::Reject(_)) => StatusCode::FORBIDDEN.into_response(),
        Ok(WSDecision::Deny(response)) => response,
        Ok(WSDecision::Accept) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        Err(_) => match outcome {
            HandlerOutcome::Close(..) => StatusCode::FORBIDDEN.into_response(),
            _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
    }
}

async fn handle_connection(
    fut: upgrade::UpgradeFut,
    py_ws_obj: Py<PyWebSocket>,
    handler_run: HandlerRun,
    teardowns: dependencies::TeardownTasks,
    route: Arc<WsRouteState>,
    tx_to_python: mpsc::Sender<WSMessage>,
    mut rx_from_python: mpsc::Receiver<WSMessage>,
) -> Result<(), SocketPumpError> {
    let ws_stream = fut.await?;
    let (read_half, write_half) = ws_stream.split(tokio::io::split);
    let reader = FragmentCollectorRead::new(read_half);
    let writer = tokio::sync::Mutex::new(write_half);
    let handler_result = match handler_run {
        HandlerRun::Finished(result) => Some(result),
        HandlerRun::Running(future) => tokio::select! {
            result = future => Some(result),
            result = socket_pump(reader, &writer, tx_to_python, &mut rx_from_python) => {
                if let Err(error) = result {
                    error!("Socket pump error: {error}");
                }
                None
            }
        },
    };
    if let Some(result) = handler_result {
        let close = match result {
            Ok(_) => Some((1000, String::new())),
            Err(error) => match handle_error(&route, &py_ws_obj, error).await {
                HandlerOutcome::Disconnected => None,
                HandlerOutcome::Handled => Some((1000, String::new())),
                HandlerOutcome::Close(code, reason) => Some((code, reason)),
                HandlerOutcome::Unhandled => Some((1011, String::new())),
            },
        };
        if let Err(error) = flush_and_close(&writer, &mut rx_from_python, close).await {
            error!("WebSocket close error: {error}");
        }
    }
    Python::attach(|py| {
        if let Ok(websocket) = py_ws_obj.try_borrow(py) {
            websocket.is_connected.store(false, Ordering::Release);
        }
    });
    execute_teardowns_async(route.event_loop, teardowns).await;
    Ok(())
}

fn validation_errors(response: &Response) -> Vec<JsonValue> {
    response
        .extensions()
        .get::<ValidationErrorDetails>()
        .and_then(|details| details.0.as_object()?.get("detail")?.as_array().cloned())
        .unwrap_or_default()
}

#[must_use]
pub fn ws_param_segments(template: &str) -> (Arc<[usize]>, usize) {
    let segments = template
        .split('/')
        .enumerate()
        .filter(|(_, s)| s.starts_with('{') && s.ends_with('}'))
        .map(|(index, _)| index)
        .collect();
    (segments, template.split('/').count())
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
    let mut params = route.param_segments.iter().peekable();

    for (index, segment) in actual_path
        .split('/')
        .enumerate()
        .take(route.segment_count)
    {
        if params.next_if_eq(&&index).is_some() {
            let start = segment.as_ptr() as usize - actual_path.as_ptr() as usize;
            ranges.push(PathParamRange {
                name_index: ranges.len(),
                start,
                end: start + segment.len(),
            });
        }
    }

    ranges
}

fn build_scope(
    py: Python<'_>,
    parts: &Parts,
    input: &RequestInput<'_>,
    app: &Bound<'_, PyAny>,
) -> Py<PyDict> {
    let scope = PyDict::new(py);
    _ = scope.set_item(intern!(py, "type"), "websocket");
    _ = scope.set_item(
        intern!(py, "asgi"),
        [("version", "3.0")].into_py_dict(py).ok(),
    );
    _ = scope.set_item(intern!(py, "http_version"), "1.1");
    _ = scope.set_item(intern!(py, "scheme"), "ws");
    _ = scope.set_item(intern!(py, "root_path"), "");
    _ = scope.set_item(intern!(py, "path"), parts.uri.path());
    _ = scope.set_item(intern!(py, "raw_path"), parts.uri.path().as_bytes());
    let subprotocols = parts
        .headers
        .get_all("sec-websocket-protocol")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    _ = scope.set_item(intern!(py, "subprotocols"), subprotocols);
    let extensions = PyDict::new(py);
    _ = extensions.set_item(intern!(py, "websocket.http.response"), PyDict::new(py));
    _ = scope.set_item(intern!(py, "extensions"), extensions);
    _ = scope.set_item(
        "query_string",
        parts.uri.query().unwrap_or_default().as_bytes(),
    );
    let headers = PyList::empty(py);
    for (name, value) in &parts.headers {
        _ = headers.append((name.as_str().as_bytes(), value.as_bytes()));
    }
    _ = scope.set_item(intern!(py, "headers"), headers);
    let path_params = PyDict::new(py);
    if let Some(params) = input.path_params.get() {
        for (name, value) in params {
            _ = path_params.set_item(*name, *value);
        }
    }
    _ = scope.set_item(intern!(py, "path_params"), path_params);
    _ = scope.set_item(intern!(py, "app"), app);
    if let Some(connect) = parts.extensions.get::<ConnectInfo<SocketAddr>>() {
        _ = scope.set_item(
            intern!(py, "client"),
            (connect.0.ip().to_string(), connect.0.port()),
        );
    } else {
        _ = scope.set_item(intern!(py, "client"), py.None());
    }
    scope.unbind()
}

type SocketReader = FragmentCollectorRead<ReadHalf<TokioIo<Upgraded>>>;
type SocketWriter = tokio::sync::Mutex<WebSocketWrite<WriteHalf<TokioIo<Upgraded>>>>;

/// Moves frames between the socket and the Python handler until either side closes.
///
/// The read loop and the write loop are separate futures. `read_frame` is not
/// cancel safe, so it must never be one branch of a `select!` that restarts.
async fn socket_pump(
    reader: SocketReader,
    writer: &SocketWriter,
    tx_to_python: mpsc::Sender<WSMessage>,
    rx_from_python: &mut mpsc::Receiver<WSMessage>,
) -> Result<(), SocketPumpError> {
    tokio::select! {
        result = read_loop(reader, writer, tx_to_python) => result,
        result = write_loop(writer, rx_from_python) => result,
    }
}

async fn read_loop(
    mut reader: SocketReader,
    writer: &SocketWriter,
    tx_to_python: mpsc::Sender<WSMessage>,
) -> Result<(), SocketPumpError> {
    let mut send_reply =
        |frame: Frame<'static>| async move { writer.lock().await.write_frame(frame).await };
    loop {
        let frame = reader
            .read_frame::<_, WebSocketError>(&mut send_reply)
            .await?;
        match frame.opcode {
            OpCode::Close => {
                let (code, reason) = close_code(&frame);
                _ = tx_to_python.send(WSMessage::Close(code, reason)).await;
                return Ok(());
            }
            OpCode::Text | OpCode::Binary => {
                let bytes = payload_into_bytes(frame.payload);
                let msg = if frame.opcode == OpCode::Text {
                    WSMessage::Text(bytes)
                } else {
                    WSMessage::Binary(bytes)
                };
                if tx_to_python.send(msg).await.is_err() {
                    return Ok(());
                }
            }
            OpCode::Ping => {
                let pong = Frame::pong(payload_into_owned(frame.payload));
                writer.lock().await.write_frame(pong).await?;
            }
            OpCode::Pong | OpCode::Continuation => {}
        }
    }
}

async fn write_loop(
    writer: &SocketWriter,
    rx_from_python: &mut mpsc::Receiver<WSMessage>,
) -> Result<(), SocketPumpError> {
    while let Some(msg) = rx_from_python.recv().await {
        let (frame, close) = message_frame(msg);
        writer.lock().await.write_frame(frame).await?;
        if close {
            return Ok(());
        }
    }
    std::future::pending().await
}

fn close_code(frame: &Frame<'_>) -> (u16, String) {
    let payload: &[u8] = frame.payload.as_ref();
    let code = payload
        .first_chunk::<2>()
        .map_or(1000, |prefix| u16::from_be_bytes(*prefix));
    let reason = payload
        .get(2..)
        .map(|reason| String::from_utf8_lossy(reason).into_owned())
        .unwrap_or_default();
    (code, reason)
}

fn close_reason(reason: &str) -> &[u8] {
    let mut end = reason.len().min(123);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    &reason.as_bytes()[..end]
}

fn message_frame(message: WSMessage) -> (Frame<'static>, bool) {
    match message {
        WSMessage::Text(bytes) => (Frame::text(bytes_into_payload(bytes)), false),
        WSMessage::Binary(data) => (Frame::binary(bytes_into_payload(data)), false),
        WSMessage::Close(code, reason) => (Frame::close(code, close_reason(&reason)), true),
    }
}

async fn flush_and_close(
    writer: &SocketWriter,
    rx_from_python: &mut mpsc::Receiver<WSMessage>,
    close: Option<(u16, String)>,
) -> Result<(), SocketPumpError> {
    while let Ok(message) = rx_from_python.try_recv() {
        let (frame, is_close) = message_frame(message);
        writer.lock().await.write_frame(frame).await?;
        if is_close {
            return Ok(());
        }
    }
    if let Some((code, reason)) = close {
        writer
            .lock()
            .await
            .write_frame(Frame::close(code, close_reason(&reason)))
            .await?;
    }
    Ok(())
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
