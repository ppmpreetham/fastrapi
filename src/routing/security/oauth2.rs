use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use smart_default::SmartDefault;

use super::callable;

#[pyclass(
    frozen,
    name = "OAuth2PasswordBearer",
    module = "fastrapi.security",
    get_all,
    from_py_object
)]
#[derive(SmartDefault, Clone, Debug)]
pub struct OAuth2PasswordBearer {
    pub token_url: String,
    pub scheme_name: Option<String>,
    pub scopes: Option<Py<PyDict>>,
    pub description: Option<String>,
    #[default(true)]
    pub auto_error: bool,
}

#[pymethods]
impl OAuth2PasswordBearer {
    #[new]
    #[pyo3(signature = (token_url, scheme_name=None, scopes=None, description=None, auto_error=true))]
    fn new(
        token_url: String,
        scheme_name: Option<String>,
        scopes: Option<Py<PyDict>>,
        description: Option<String>,
        auto_error: bool,
    ) -> Self {
        Self {
            token_url,
            scheme_name,
            scopes,
            description,
            auto_error,
        }
    }

    /// Returns the extracted bearer token or raises 401/403.
    fn __call__(&self, request: &Bound<'_, PyAny>) -> Result<String, PyErr> {
        callable::bearer_token(request, self.auto_error)
    }
}

#[pyclass(
    frozen,
    name = "OAuth2AuthorizationCodeBearer",
    module = "fastrapi.security",
    get_all,
    from_py_object
)]
#[derive(SmartDefault, Clone, Debug)]
pub struct OAuth2AuthorizationCodeBearer {
    pub authorization_url: String,
    pub token_url: String,
    pub refresh_url: Option<String>,
    pub scopes: Option<Py<PyDict>>,
    pub scheme_name: Option<String>,
    pub description: Option<String>,
    #[default(true)]
    pub auto_error: bool,
}

#[pymethods]
impl OAuth2AuthorizationCodeBearer {
    #[new]
    #[pyo3(signature = (authorization_url, token_url, refresh_url=None, scopes=None, *, scheme_name=None, description=None, auto_error=true))]
    fn new(
        authorization_url: String,
        token_url: String,
        refresh_url: Option<String>,
        scopes: Option<Py<PyDict>>,
        scheme_name: Option<String>,
        description: Option<String>,
        auto_error: bool,
    ) -> Self {
        Self {
            authorization_url,
            token_url,
            refresh_url,
            scopes,
            scheme_name,
            description,
            auto_error,
        }
    }

    fn __call__(&self, request: &Bound<'_, PyAny>) -> Result<String, PyErr> {
        callable::bearer_token(request, self.auto_error)
    }
}

#[pyclass(
    frozen,
    name = "OpenIdConnect",
    module = "fastrapi.security",
    get_all,
    from_py_object
)]
#[derive(SmartDefault, Clone, Debug)]
pub struct OpenIdConnect {
    pub openid_connect_url: String,
    pub scheme_name: Option<String>,
    pub description: Option<String>,
    #[default(true)]
    pub auto_error: bool,
}

#[pymethods]
impl OpenIdConnect {
    #[new]
    #[pyo3(signature = (openid_connect_url, scheme_name=None, description=None, auto_error=true))]
    fn new(
        openid_connect_url: String,
        scheme_name: Option<String>,
        description: Option<String>,
        auto_error: bool,
    ) -> Self {
        Self {
            openid_connect_url,
            scheme_name,
            description,
            auto_error,
        }
    }

    /// OpenID Connect carries the identity token as a bearer token
    fn __call__(&self, request: &Bound<'_, PyAny>) -> Result<String, PyErr> {
        callable::bearer_token(request, self.auto_error)
    }
}
