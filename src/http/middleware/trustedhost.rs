use pyo3::prelude::*;
use pyo3::types::PyDict;
use smart_default::SmartDefault;

#[pyclass(
    frozen,
    name = "TrustedHostMiddleware",
    get_all,
    from_py_object,
    eq,
    new = "from_fields"
)]
#[derive(SmartDefault, Clone, Debug, PartialEq, Eq)]
pub struct TrustedHostMiddleware {
    #[default(vec!["*".to_string()])]
    pub allowed_hosts: Vec<String>,

    #[default(true)]
    pub www_redirect: bool,
}

pub fn parse_trusted_host_params(kwargs: &Bound<'_, PyDict>) -> PyResult<TrustedHostMiddleware> {
    kwargs
        .extract::<TrustedHostMiddleware>()
        .map_err(Into::into)
}
