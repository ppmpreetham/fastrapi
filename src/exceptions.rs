// HTTPException(status_code, detail=None, headers=None)

use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::json;

#[pyclass(name = "HTTPException")]
#[derive(Clone)]
/// An HTTP exception you can raise in your own code to show errors to the client.
/// This is for client errors, invalid authentication, invalid data, etc. Not for server
/// errors in your code.
///
/// Read more about it in the
/// [FastAPI docs for Handling Errors](https://fastapi.tiangolo.com/tutorial/handling-errors/).
/// ## Example
/// ```python
/// from fastapi import FastAPI, HTTPException
/// app = FastAPI()
/// items = {"foo": "The Foo Wrestlers"}
/// @app.get("/items/{item_id}")
/// async def read_item(item_id: str):
///     if item_id not in items:
///         raise HTTPException(status_code=404, detail="Item not found")
///     return {"item": items[item_id]}
/// ```
pub struct PyHTTPException {
    #[pyo3(get)]
    pub status_code: u16,
    #[pyo3(get)]
    pub detail: Option<String>,
    #[pyo3(get)]
    pub headers: Option<Py<PyDict>>,
}

#[pymethods]
impl PyHTTPException {
    #[new]
    #[pyo3(signature = (status_code, detail=None, headers=None))]
    fn new(status_code: u16, detail: Option<String>, headers: Option<Py<PyDict>>) -> Self {
        Self {
            status_code,
            detail,
            headers,
        }
    }

    fn __str__(&self) -> String {
        format!(
            "{}: {}",
            self.status_code,
            self.detail.as_deref().unwrap_or("No details provided")
        )
    }

    fn __repr__(&self) -> String {
        format!(
            "HTTPException(status_code={}, detail={:?})",
            self.status_code, self.detail
        )
    }
}

impl PyHTTPException {
    pub fn to_response(&self) -> Response {
        let status =
            StatusCode::from_u16(self.status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let detail = self
            .detail
            .clone()
            .unwrap_or_else(|| "An error occurred".to_string());

        (status, Json(json!({ "detail": detail }))).into_response()
    }
}

// WebSocketException(code, reason=None)

#[pyclass(name = "WebSocketException")]
#[derive(Clone)]
pub struct PyWebSocketException {
    #[pyo3(get)]
    pub code: u16,
    #[pyo3(get)]
    pub reason: Option<String>,
}

#[pymethods]
impl PyWebSocketException {
    #[new]
    #[pyo3(signature = (code, reason=None))]
    fn new(code: u16, reason: Option<String>) -> Self {
        Self { code, reason }
    }

    fn __str__(&self) -> String {
        format!(
            "{}: {}",
            self.code,
            self.reason.as_deref().unwrap_or("No reason provided")
        )
    }

    fn __repr__(&self) -> String {
        format!(
            "WebSocketException(code={}, reason={:?})",
            self.code, self.reason
        )
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyHTTPException>()?;
    m.add_class::<PyWebSocketException>()?;
    Ok(())
}
