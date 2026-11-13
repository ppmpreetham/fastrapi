//! This module contains the `HTTPSRedirectMiddleware` class.
//! This middleware redirects HTTP requests to HTTPS.

use pyo3::prelude::*;
use pyo3::types::PyDict;

use super::{AsgiApp, asgi_middleware};

#[pyclass(frozen, name = "HTTPSRedirectMiddleware", get_all, from_py_object, eq)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HTTPSRedirectMiddleware {
    pub app: AsgiApp,
}

#[pymethods]
impl HTTPSRedirectMiddleware {
    #[new]
    #[pyo3(signature = (app=None))]
    fn new(app: Option<&Bound<'_, PyAny>>) -> Self {
        Self {
            app: AsgiApp::new(app),
        }
    }
}

asgi_middleware!(
    HTTPSRedirectMiddleware,
    "starlette.middleware.httpsredirect",
    "HTTPSRedirectMiddleware",
    []
);

pub fn parse_https_redirect_params(
    _kwargs: &Bound<'_, PyDict>,
) -> PyResult<HTTPSRedirectMiddleware> {
    Ok(HTTPSRedirectMiddleware::default())
}
