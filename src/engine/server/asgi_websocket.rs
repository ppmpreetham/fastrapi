//! WebSocket connections that arrive through the ASGI entry point, for example when
//! the app is mounted in another ASGI app. Messages travel through the ASGI `receive`
//! and `send` callables instead of a socket.

use super::websocket::{SessionIo, SocketPumpError};
use axum::http::HeaderMap;
use axum::http::header::SEC_WEBSOCKET_PROTOCOL;
use bytes::Bytes;
use fastrapi_http::middleware::asgi::{asgi_call, asgi_receive};
use fastrapi_http::websocket::WSMessage;
use fastrapi_runtime::py_bridge::TaskLocals;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyString};
use std::sync::Arc;
use tokio::sync::mpsc;

/// The ASGI callables of one WebSocket connection, carried as a request extension.
/// Request extensions are cloned without the interpreter, so the callables are shared.
#[derive(Clone)]
pub(crate) struct AsgiSocket(Arc<Callables>);

struct Callables {
    locals: TaskLocals,
    receive: Py<PyAny>,
    send: Py<PyAny>,
}

impl AsgiSocket {
    pub(crate) fn new(locals: TaskLocals, receive: Py<PyAny>, send: Py<PyAny>) -> Self {
        Self(Arc::new(Callables {
            locals,
            receive,
            send,
        }))
    }

    pub(crate) fn locals(&self) -> &TaskLocals {
        &self.0.locals
    }

    pub(crate) fn send_callable(&self) -> &Py<PyAny> {
        &self.0.send
    }

    pub(crate) async fn receive(&self) -> PyResult<Py<PyAny>> {
        asgi_receive(&self.0.locals, &self.0.receive).await
    }

    pub(crate) async fn send_message(&self, message: Py<PyAny>) -> PyResult<()> {
        asgi_call(&self.0.locals, &self.0.send, message).await
    }

    pub(crate) async fn accept(&self, headers: &HeaderMap) -> PyResult<()> {
        let message = Python::attach(|py| -> PyResult<_> {
            let message = message(py, "websocket.accept")?;
            let subprotocol = headers
                .get(SEC_WEBSOCKET_PROTOCOL)
                .and_then(|value| value.to_str().ok());
            message.set_item(intern!(py, "subprotocol"), subprotocol)?;
            let raw = PyList::empty(py);
            for (name, value) in headers.iter().filter(|(name, _)| **name != SEC_WEBSOCKET_PROTOCOL) {
                raw.append((
                    PyBytes::new(py, name.as_str().as_bytes()),
                    PyBytes::new(py, value.as_bytes()),
                ))?;
            }
            message.set_item(intern!(py, "headers"), raw)?;
            Ok(message.into_any().unbind())
        })?;
        self.send_message(message).await
    }

    pub(crate) async fn close(&self, code: u16, reason: &str) -> PyResult<()> {
        let message = Python::attach(|py| close_message(py, code, reason))?;
        self.send_message(message).await
    }

    async fn deliver(&self, message: WSMessage) -> Result<(), SocketPumpError> {
        let message = Python::attach(|py| outgoing(py, message))?;
        Ok(self.send_message(message).await?)
    }

    async fn read_loop(&self, tx_to_python: mpsc::Sender<WSMessage>) -> Result<(), SocketPumpError> {
        loop {
            let received = self.receive().await?;
            let message = Python::attach(|py| incoming(received.bind(py)))?;
            let closed = matches!(message, WSMessage::Close(..));
            if tx_to_python.send(message).await.is_err() || closed {
                return Ok(());
            }
        }
    }

    async fn write_loop(&self, rx_from_python: &mut mpsc::Receiver<WSMessage>) -> Result<(), SocketPumpError> {
        while let Some(message) = rx_from_python.recv().await {
            let closed = matches!(message, WSMessage::Close(..));
            self.deliver(message).await?;
            if closed {
                return Ok(());
            }
        }
        std::future::pending().await
    }
}

impl SessionIo for AsgiSocket {
    async fn pump(
        &mut self,
        tx_to_python: mpsc::Sender<WSMessage>,
        rx_from_python: &mut mpsc::Receiver<WSMessage>,
    ) -> Result<(), SocketPumpError> {
        tokio::select! {
            result = self.read_loop(tx_to_python) => result,
            result = self.write_loop(rx_from_python) => result,
        }
    }

    async fn send(&mut self, message: WSMessage) -> Result<(), SocketPumpError> {
        self.deliver(message).await
    }
}

fn message<'py>(py: Python<'py>, kind: &'static str) -> PyResult<Bound<'py, PyDict>> {
    let message = PyDict::new(py);
    message.set_item(intern!(py, "type"), kind)?;
    Ok(message)
}

fn close_message(py: Python<'_>, code: u16, reason: &str) -> PyResult<Py<PyAny>> {
    let message = message(py, "websocket.close")?;
    message.set_item(intern!(py, "code"), code)?;
    message.set_item(intern!(py, "reason"), reason)?;
    Ok(message.into_any().unbind())
}

fn outgoing(py: Python<'_>, message: WSMessage) -> PyResult<Py<PyAny>> {
    match message {
        WSMessage::Text(text) => {
            let sent = self::message(py, "websocket.send")?;
            sent.set_item(intern!(py, "text"), PyString::new(py, &String::from_utf8_lossy(&text)))?;
            Ok(sent.into_any().unbind())
        }
        WSMessage::Binary(data) => {
            let sent = self::message(py, "websocket.send")?;
            sent.set_item(intern!(py, "bytes"), PyBytes::new(py, &data))?;
            Ok(sent.into_any().unbind())
        }
        WSMessage::Close(code, reason) => close_message(py, code, &reason),
    }
}

fn incoming<'py>(received: &Bound<'py, PyAny>) -> PyResult<WSMessage> {
    let py = received.py();
    let field = |key: &Bound<'py, PyString>| -> Option<Bound<'py, PyAny>> {
        received.get_item(key).ok().filter(|value| !value.is_none())
    };
    let kind: String = received.get_item(intern!(py, "type"))?.extract()?;
    if kind == "websocket.disconnect" {
        let code = field(intern!(py, "code")).map_or(Ok(1005), |code| code.extract())?;
        let reason = field(intern!(py, "reason")).map_or(Ok(String::new()), |reason| reason.extract())?;
        return Ok(WSMessage::Close(code, reason));
    }
    if let Some(text) = field(intern!(py, "text")) {
        return Ok(WSMessage::Text(Bytes::from(text.extract::<String>()?)));
    }
    let data = field(intern!(py, "bytes")).map_or(Ok(Vec::new()), |data| data.extract::<Vec<u8>>())?;
    Ok(WSMessage::Binary(Bytes::from(data)))
}
