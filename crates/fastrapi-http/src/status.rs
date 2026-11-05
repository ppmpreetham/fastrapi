//! This module contains the HTTP status helpers.
//! It also makes the `status` Python submodule, which is `starlette.status`, the same as `fastapi.status`.

use pyo3::prelude::*;
use pyo3::types::PyModule;

#[must_use]
pub const fn body_allowed(status: u16) -> bool {
    !(status < 200 || matches!(status, 204 | 205 | 304))
}

pub fn create_status_submodule(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    let status_module = py.import("starlette.status")?;
    parent.add("status", &status_module)?;
    fastrapi_pyutil::imports::register_module(py, "fastrapi.status", &status_module)
}
