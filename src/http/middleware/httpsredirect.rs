use pyo3::prelude::*;
use pyo3::types::PyDict;

#[pyclass(name = "HTTPSRedirectMiddleware", skip_from_py_object)]
#[derive(Clone, Debug, Default)]
pub struct HTTPSRedirectMiddleware;

#[pymethods]
impl HTTPSRedirectMiddleware {
    #[new]
    fn new() -> Self {
        Self {}
    }
}

pub fn parse_https_redirect_params(
    _kwargs: &Bound<'_, PyDict>,
) -> PyResult<HTTPSRedirectMiddleware> {
    Ok(HTTPSRedirectMiddleware)
}
