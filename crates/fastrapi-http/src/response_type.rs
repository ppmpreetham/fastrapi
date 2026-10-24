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
}

#[derive(Clone)]
pub struct ResponseFormatter {
    pub response_type: ResponseType,
    pub serialization_hint: SerializationHint,
    pub default_status: Option<StatusCode>,
    pub response_model: Option<Py<PyAny>>,
    pub response_class: Option<Py<PyAny>>,
    pub dump_options: Option<Py<PyDict>>,
}
