use pyo3::prelude::*;
use pyo3::types::PyDict;
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
    name = "HTTPAuthorizationCredentials",
    module = "fastrapi.security",
    from_py_object
)]
#[derive(Clone, Debug)]
pub struct HTTPAuthorizationCredentials {
    #[pyo3(get)]
    pub scheme: String,
    #[pyo3(get)]
    pub credentials: String,
}

#[pymethods]
impl HTTPAuthorizationCredentials {
    #[new]
    fn new(scheme: String, credentials: String) -> Self {
        Self {
            scheme,
            credentials,
        }
    }
}

#[pyclass(
    name = "HTTPBasicCredentials",
    module = "fastrapi.security",
    from_py_object
)]
#[derive(Clone, Debug)]
pub struct HTTPBasicCredentials {
    #[pyo3(get)]
    pub username: String,
    #[pyo3(get)]
    pub password: String,
}

#[pymethods]
impl HTTPBasicCredentials {
    #[new]
    fn new(username: String, password: String) -> Self {
        Self { username, password }
    }
}

#[pyclass(
    name = "OAuth2PasswordBearer",
    module = "fastrapi.security",
    from_py_object
)]
#[derive(Clone, Debug)]
pub struct OAuth2PasswordBearer {
    #[pyo3(get)]
    pub token_url: String,
    #[pyo3(get)]
    pub scheme_name: Option<String>,
    #[pyo3(get)]
    pub scopes: Option<Py<PyDict>>,
    #[pyo3(get)]
    pub description: Option<String>,
    #[pyo3(get)]
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

#[pyclass(name = "HTTPBearer", module = "fastrapi.security", from_py_object)]
#[derive(Clone, Debug)]
pub struct HTTPBearer {
    #[pyo3(get)]
    pub bearer_format: Option<String>,
    #[pyo3(get)]
    pub scheme_name: Option<String>,
    #[pyo3(get)]
    pub description: Option<String>,
    #[pyo3(get)]
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

#[pyclass(name = "HTTPBasic", module = "fastrapi.security", from_py_object)]
#[derive(Clone, Debug)]
pub struct HTTPBasic {
    #[pyo3(get)]
    pub scheme_name: Option<String>,
    #[pyo3(get)]
    pub description: Option<String>,
    #[pyo3(get)]
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

#[pyclass(name = "APIKeyHeader", module = "fastrapi.security", from_py_object)]
#[derive(Clone, Debug)]
pub struct APIKeyHeader {
    #[pyo3(get)]
    pub name: String,
    #[pyo3(get)]
    pub scheme_name: Option<String>,
    #[pyo3(get)]
    pub description: Option<String>,
    #[pyo3(get)]
    pub auto_error: bool,
}

#[pymethods]
impl APIKeyHeader {
    #[new]
    #[pyo3(signature = (*, name, scheme_name=None, description=None, auto_error=true))]
    fn new(
        name: String,
        scheme_name: Option<String>,
        description: Option<String>,
        auto_error: bool,
    ) -> Self {
        Self {
            name,
            scheme_name,
            description,
            auto_error,
        }
    }
}

#[pyclass(name = "APIKeyQuery", module = "fastrapi.security", from_py_object)]
#[derive(Clone, Debug)]
pub struct APIKeyQuery {
    #[pyo3(get)]
    pub name: String,
    #[pyo3(get)]
    pub scheme_name: Option<String>,
    #[pyo3(get)]
    pub description: Option<String>,
    #[pyo3(get)]
    pub auto_error: bool,
}

#[pymethods]
impl APIKeyQuery {
    #[new]
    #[pyo3(signature = (*, name, scheme_name=None, description=None, auto_error=true))]
    fn new(
        name: String,
        scheme_name: Option<String>,
        description: Option<String>,
        auto_error: bool,
    ) -> Self {
        Self {
            name,
            scheme_name,
            description,
            auto_error,
        }
    }
}

#[pyclass(name = "APIKeyCookie", module = "fastrapi.security", from_py_object)]
#[derive(Clone, Debug)]
pub struct APIKeyCookie {
    #[pyo3(get)]
    pub name: String,
    #[pyo3(get)]
    pub scheme_name: Option<String>,
    #[pyo3(get)]
    pub description: Option<String>,
    #[pyo3(get)]
    pub auto_error: bool,
}

#[pymethods]
impl APIKeyCookie {
    #[new]
    #[pyo3(signature = (*, name, scheme_name=None, description=None, auto_error=true))]
    fn new(
        name: String,
        scheme_name: Option<String>,
        description: Option<String>,
        auto_error: bool,
    ) -> Self {
        Self {
            name,
            scheme_name,
            description,
            auto_error,
        }
    }
}
