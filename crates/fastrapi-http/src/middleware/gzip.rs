//! This module contains the `GZipMiddleware` class.
//! It reads the gzip compression configuration from Python keyword arguments.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use smart_default::SmartDefault;

#[pyclass(frozen, name = "GZipMiddleware", get_all, from_py_object, eq)]
#[derive(SmartDefault, Clone, Debug, PartialEq, Eq)]
pub struct GZipMiddleware {
    #[default(500)]
    pub minimum_size: u32,
    #[default(9)]
    pub compresslevel: u32,
}

#[pymethods]
impl GZipMiddleware {
    #[new]
    #[pyo3(signature = (*, minimum_size=500, compresslevel=9))]
    const fn new(minimum_size: u32, compresslevel: u32) -> Self {
        Self {
            minimum_size,
            compresslevel,
        }
    }
}

pub fn parse_gzip_params(kwargs: &Bound<'_, PyDict>) -> PyResult<GZipMiddleware> {
    kwargs
        .py()
        .get_type::<GZipMiddleware>()
        .call((), Some(kwargs))?
        .extract()
        .map_err(Into::into)
}
