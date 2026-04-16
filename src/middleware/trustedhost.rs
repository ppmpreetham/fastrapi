use pyo3::prelude::*;
use pyo3::types::PyDict;

#[pyclass(name = "TrustedHostMiddleware", skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct TrustedHostMiddleware {
    pub allowed_hosts: Vec<String>,
    pub www_redirect: bool,
}

impl Default for TrustedHostMiddleware {
    fn default() -> Self {
        Self {
            allowed_hosts: vec!["*".to_string()],
            www_redirect: true,
        }
    }
}

#[pymethods]
impl TrustedHostMiddleware {
    #[new]
    #[pyo3(signature = (allowed_hosts=None, www_redirect=true))]
    fn new(allowed_hosts: Option<Vec<String>>, www_redirect: bool) -> Self {
        Self {
            allowed_hosts: allowed_hosts.unwrap_or_else(|| vec!["*".to_string()]),
            www_redirect,
        }
    }
}

pub fn parse_trusted_host_params(kwargs: &Bound<'_, PyDict>) -> PyResult<TrustedHostMiddleware> {
    let mut config = TrustedHostMiddleware::default();
    set_field!(kwargs, config, "allowed_hosts", allowed_hosts: Vec<String>);
    set_field!(kwargs, config, "www_redirect", www_redirect: bool);
    Ok(config)
}
