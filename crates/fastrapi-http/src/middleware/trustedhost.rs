//! This module contains the `TrustedHostMiddleware` class.
//! It reads the list of permitted host names from Python keyword arguments.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use smart_default::SmartDefault;

use super::{AsgiApp, asgi_middleware};

#[pyclass(frozen, name = "TrustedHostMiddleware", get_all, from_py_object, eq)]
#[derive(SmartDefault, Clone, Debug, PartialEq, Eq)]
pub struct TrustedHostMiddleware {
    pub app: AsgiApp,

    #[default(vec!["*".to_string()])]
    pub allowed_hosts: Vec<String>,

    #[default(true)]
    pub www_redirect: bool,
}

#[pymethods]
impl TrustedHostMiddleware {
    #[new]
    #[pyo3(signature = (app=None, allowed_hosts=None, www_redirect=true))]
    fn new(
        app: Option<&Bound<'_, PyAny>>,
        allowed_hosts: Option<Vec<String>>,
        www_redirect: bool,
    ) -> Self {
        Self {
            app: AsgiApp::new(app),
            allowed_hosts: allowed_hosts.unwrap_or_else(|| vec!["*".to_string()]),
            www_redirect,
        }
    }
}

asgi_middleware!(
    TrustedHostMiddleware,
    "starlette.middleware.trustedhost",
    "TrustedHostMiddleware",
    ["allowed_hosts", "www_redirect"]
);

pub fn parse_trusted_host_params(kwargs: &Bound<'_, PyDict>) -> PyResult<TrustedHostMiddleware> {
    kwargs
        .py()
        .get_type::<TrustedHostMiddleware>()
        .call((), Some(kwargs))?
        .extract()
        .map_err(Into::into)
}
