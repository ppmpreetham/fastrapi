use pyo3::prelude::*;
use pyo3::types::PyDict;

#[pyclass(
    frozen,
    new = "from_fields",
    name = "HTTPSRedirectMiddleware",
    from_py_object,
    eq
)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HTTPSRedirectMiddleware;

pub fn parse_https_redirect_params(
    _kwargs: &Bound<'_, PyDict>,
) -> PyResult<HTTPSRedirectMiddleware> {
    Ok(HTTPSRedirectMiddleware)
}
