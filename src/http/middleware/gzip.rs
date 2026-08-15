use pyo3::prelude::*;
use pyo3::types::PyDict;
use smart_default::SmartDefault;

#[pyclass(
    frozen,
    name = "GZipMiddleware",
    get_all,
    from_py_object,
    eq,
    new = "from_fields"
)]
#[derive(SmartDefault, Clone, Debug, PartialEq, Eq)]
pub struct GZipMiddleware {
    #[default(500)]
    pub minimum_size: u32,
    #[default(9)]
    pub compresslevel: u32,
}

pub fn parse_gzip_params(kwargs: &Bound<'_, PyDict>) -> PyResult<GZipMiddleware> {
    kwargs.extract::<GZipMiddleware>().map_err(Into::into)
}
