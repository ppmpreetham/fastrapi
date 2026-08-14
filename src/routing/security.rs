use pyo3::prelude::*;
use pyo3::types::PyDict;
use smart_default::SmartDefault;
use std::sync::Arc;

#[pyclass(
    name = "SecurityScopes",
    module = "fastrapi.security",
    skip_from_py_object
)]
#[derive(Clone, Debug)]
pub struct PySecurityScopes {
    pub scopes: Arc<[String]>,
}

#[pymethods]
impl PySecurityScopes {
    #[new]
    #[pyo3(signature = (scopes=None))]
    pub fn new(scopes: Option<Vec<String>>) -> Self {
        Self {
            scopes: scopes.unwrap_or_default().into(),
        }
    }

    #[getter]
    fn scopes(&self) -> Vec<String> {
        self.scopes.to_vec()
    }

    #[getter]
    fn scope_str(&self) -> String {
        self.scopes.join(" ")
    }
}

#[pyclass(
    frozen,
    new = "from_fields",
    name = "HTTPAuthorizationCredentials",
    module = "fastrapi.security",
    get_all,
    from_py_object,
    eq
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HTTPAuthorizationCredentials {
    pub scheme: String,
    pub credentials: String,
}

#[pyclass(
    frozen,
    new = "from_fields",
    name = "HTTPBasicCredentials",
    module = "fastrapi.security",
    get_all,
    from_py_object,
    eq
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HTTPBasicCredentials {
    pub username: String,
    pub password: String,
}

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
}

#[pyclass(
    frozen,
    name = "HTTPBearer",
    module = "fastrapi.security",
    get_all,
    from_py_object,
    eq
)]
#[derive(SmartDefault, Clone, Debug, PartialEq, Eq)]
pub struct HTTPBearer {
    pub bearer_format: Option<String>,
    pub scheme_name: Option<String>,
    pub description: Option<String>,
    #[default(true)]
    pub auto_error: bool,
}

#[pymethods]
impl HTTPBearer {
    #[new]
    #[pyo3(signature = (*, bearer_format=None, scheme_name=None, description=None, auto_error=true))]
    fn new(
        bearer_format: Option<String>,
        scheme_name: Option<String>,
        description: Option<String>,
        auto_error: bool,
    ) -> Self {
        Self {
            bearer_format,
            scheme_name,
            description,
            auto_error,
        }
    }
}

#[pyclass(
    frozen,
    name = "HTTPBasic",
    module = "fastrapi.security",
    get_all,
    from_py_object,
    eq
)]
#[derive(SmartDefault, Clone, Debug, PartialEq, Eq)]
pub struct HTTPBasic {
    pub scheme_name: Option<String>,
    pub description: Option<String>,
    #[default(true)]
    pub auto_error: bool,
}

#[pymethods]
impl HTTPBasic {
    #[new]
    #[pyo3(signature = (*, scheme_name=None, description=None, auto_error=true))]
    fn new(scheme_name: Option<String>, description: Option<String>, auto_error: bool) -> Self {
        Self {
            scheme_name,
            description,
            auto_error,
        }
    }
}

crate::define_api_key_security!(APIKeyHeader, "APIKeyHeader");
crate::define_api_key_security!(APIKeyQuery, "APIKeyQuery");
crate::define_api_key_security!(APIKeyCookie, "APIKeyCookie");
