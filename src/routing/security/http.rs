use pyo3::prelude::*;
use pyo3::types::PyAny;
use smart_default::SmartDefault;

use super::callable;

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

    fn __call__(
        &self,
        request: &Bound<'_, PyAny>,
    ) -> PyResult<super::HTTPAuthorizationCredentials> {
        callable::http_bearer_call(self.auto_error, self.bearer_format.as_deref(), request)
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

    fn __call__(&self, request: &Bound<'_, PyAny>) -> PyResult<super::HTTPBasicCredentials> {
        callable::http_basic_call(self.auto_error, request)
    }
}

#[pyclass(
    frozen,
    name = "HTTPDigest",
    module = "fastrapi.security",
    get_all,
    from_py_object,
    eq
)]
#[derive(SmartDefault, Clone, Debug, PartialEq, Eq)]
pub struct HTTPDigest {
    pub scheme_name: Option<String>,
    pub description: Option<String>,
    #[default(true)]
    pub auto_error: bool,
}

#[pymethods]
impl HTTPDigest {
    #[new]
    #[pyo3(signature = (*, scheme_name=None, description=None, auto_error=true))]
    fn new(scheme_name: Option<String>, description: Option<String>, auto_error: bool) -> Self {
        Self {
            scheme_name,
            description,
            auto_error,
        }
    }

    fn __call__(&self, request: &Bound<'_, PyAny>) -> Result<String, PyErr> {
        let header = request
            .getattr("headers")
            .ok()
            .and_then(|headers| headers.call_method1("get", ("authorization",)).ok())
            .and_then(|value| value.extract::<String>().ok())
            .filter(|value| !value.is_empty());

        match header {
            Some(header) => Ok(header),
            None if self.auto_error => Err(callable::unauthorized(request.py())),
            None => Ok(String::new()),
        }
    }
}

#[pyclass(
    frozen,
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
