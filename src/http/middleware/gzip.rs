use pyo3::prelude::*;
use pyo3::types::PyDict;

#[pyclass(name = "GZipMiddleware", skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct GZipMiddleware {
    pub minimum_size: u32,
    pub compresslevel: u32,
}

impl Default for GZipMiddleware {
    fn default() -> Self {
        Self {
            minimum_size: 500,
            compresslevel: 9,
        }
    }
}

#[pymethods]
impl GZipMiddleware {
    #[new]
    #[pyo3(signature = (minimum_size=500, compresslevel=9))]
    fn new(minimum_size: u32, compresslevel: u32) -> Self {
        Self {
            minimum_size,
            compresslevel,
        }
    }
}

pub fn parse_gzip_params(kwargs: &Bound<'_, PyDict>) -> PyResult<GZipMiddleware> {
    let mut config = GZipMiddleware::default();
    set_field!(kwargs, config, "minimum_size", minimum_size: u32);
    set_field!(kwargs, config, "compresslevel", compresslevel: u32);
    Ok(config)
}
