//! This module contains `ResponseType` and `ResponseFormatter`.
//! They tell the server how to serialize the return value of a route and which status code to use.

use axum::http::StatusCode;
use fastrapi_pyutil::json::SerializationHint;
use pyo3::types::PyDict;
use pyo3::{Py, PyAny};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseType {
    Json,
    Html,
    PlainText,
    Redirect,
    File,
    Auto,
    /// A `response_class` other than the five built-ins: a plain return value
    /// becomes an instance of it, so its own `render` and `media_type` apply.
    Custom,
}

/// The validator and serializer of a route's response model, taken from its `TypeAdapter`
/// when the route is defined so a request calls them without a Python frame in between.
pub struct ResponseModelCodec {
    /// `validator.validate_python`.
    pub validate: Py<PyAny>,
    /// `serializer.to_json`.
    pub to_json: Py<PyAny>,
    /// `serializer.to_python`.
    pub to_python: Py<PyAny>,
    /// `from_attributes=True`.
    pub validate_options: Py<PyDict>,
    /// The `response_model_*` options.
    pub json_options: Py<PyDict>,
    /// The `response_model_*` options with `mode="json"`.
    pub python_options: Py<PyDict>,
}

#[derive(Clone)]
pub struct ResponseFormatter {
    pub response_type: ResponseType,
    pub serialization_hint: SerializationHint,
    pub default_status: Option<StatusCode>,
    pub response_model: Option<Py<PyAny>>,
    pub response_class: Option<Py<PyAny>>,
    pub dump_options: Option<Py<PyDict>>,
    pub response_adapter: std::sync::Arc<pyo3::sync::PyOnceLock<ResponseModelCodec>>,
    pub stream_factory: Option<Py<PyAny>>,
}
