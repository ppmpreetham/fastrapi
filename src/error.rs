use pyo3::PyErr;
use pyo3::exceptions::PyRuntimeError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum FastRapiError {
    #[error("WebSocket Error: {0}")]
    WebSocketError(#[from] fastwebsockets::WebSocketError),

    #[error("Internal Error: {0}")]
    InternalError(String),

    #[error("Python Error: {0}")]
    PythonError(#[from] pyo3::PyErr),

    #[error("Mutex Poisoned")]
    MutexPoisoned,

    #[error("JSON Serialization Error: {0}")]
    JsonError(#[from] simd_json::Error),

    #[error("IO Error: {0}")]
    IoError(#[from] std::io::Error),
}

impl From<FastRapiError> for PyErr {
    fn from(err: FastRapiError) -> PyErr {
        PyRuntimeError::new_err(err.to_string())
    }
}
