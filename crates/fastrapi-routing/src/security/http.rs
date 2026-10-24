//! The HTTP security classes `HTTPBearer`, `HTTPBasic`, and `HTTPDigest`, and their credential classes.
//! Each security class reads the `Authorization` header and gives the credentials to the endpoint.

use pyo3::prelude::*;
use pyo3::types::PyAny;
use smart_default::SmartDefault;

use super::callable;

/// HTTP Bearer token authentication.
///
/// ## Usage
///
/// Create an instance object and use that object as the dependency in `Depends()`.
///
/// The dependency result will be an `HTTPAuthorizationCredentials` object containing
/// the `scheme` and the `credentials`.
///
/// ## Example
///
/// ```python
/// from typing import Annotated
///
/// from fastrapi import Depends, FastAPI
/// from fastrapi.security import HTTPAuthorizationCredentials, HTTPBearer
///
/// app = FastAPI()
///
/// security = HTTPBearer()
///
///
/// @app.get("/users/me")
/// def read_current_user(
///     credentials: Annotated[HTTPAuthorizationCredentials, Depends(security)]
/// ):
///     return {"scheme": credentials.scheme, "credentials": credentials.credentials}
/// ```
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
    const fn new(
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

    /// Call self as a function.
    fn __call__(
        &self,
        request: &Bound<'_, PyAny>,
    ) -> PyResult<super::HTTPAuthorizationCredentials> {
        callable::http_bearer_call(self.auto_error, self.bearer_format.as_deref(), request)
    }
}

/// HTTP Basic authentication.
///
/// Ref: https://datatracker.ietf.org/doc/html/rfc7617
///
/// ## Usage
///
/// Create an instance object and use that object as the dependency in `Depends()`.
///
/// The dependency result will be an `HTTPBasicCredentials` object containing the
/// `username` and the `password`.
///
/// Read more about it in the
/// [FastAPI docs for HTTP Basic Auth](https://fastapi.tiangolo.com/advanced/security/http-basic-auth/).
///
/// ## Example
///
/// ```python
/// from typing import Annotated
///
/// from fastrapi import Depends, FastAPI
/// from fastrapi.security import HTTPBasic, HTTPBasicCredentials
///
/// app = FastAPI()
///
/// security = HTTPBasic()
///
///
/// @app.get("/users/me")
/// def read_current_user(credentials: Annotated[HTTPBasicCredentials, Depends(security)]):
///     return {"username": credentials.username, "password": credentials.password}
/// ```
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
    pub realm: Option<String>,
    pub description: Option<String>,
    #[default(true)]
    pub auto_error: bool,
}

#[pymethods]
impl HTTPBasic {
    #[new]
    #[pyo3(signature = (*, scheme_name=None, realm=None, description=None, auto_error=true))]
    const fn new(
        scheme_name: Option<String>,
        realm: Option<String>,
        description: Option<String>,
        auto_error: bool,
    ) -> Self {
        Self {
            scheme_name,
            realm,
            description,
            auto_error,
        }
    }

    /// Call self as a function.
    fn __call__(&self, request: &Bound<'_, PyAny>) -> PyResult<super::HTTPBasicCredentials> {
        callable::http_basic_call(self.auto_error, self.realm.as_deref(), request)
    }
}

/// HTTP Digest authentication.
///
/// **Warning**: this is only a stub to connect the components with OpenAPI in FastAPI,
/// but it doesn't implement the full Digest scheme, you would need to subclass it
/// and implement it in your code.
///
/// Ref: https://datatracker.ietf.org/doc/html/rfc7616
///
/// ## Usage
///
/// Create an instance object and use that object as the dependency in `Depends()`.
///
/// The dependency result will be an `HTTPAuthorizationCredentials` object containing
/// the `scheme` and the `credentials`.
///
/// ## Example
///
/// ```python
/// from typing import Annotated
///
/// from fastrapi import Depends, FastAPI
/// from fastrapi.security import HTTPAuthorizationCredentials, HTTPDigest
///
/// app = FastAPI()
///
/// security = HTTPDigest()
///
///
/// @app.get("/users/me")
/// def read_current_user(
///     credentials: Annotated[HTTPAuthorizationCredentials, Depends(security)]
/// ):
///     return {"scheme": credentials.scheme, "credentials": credentials.credentials}
/// ```
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
    const fn new(
        scheme_name: Option<String>,
        description: Option<String>,
        auto_error: bool,
    ) -> Self {
        Self {
            scheme_name,
            description,
            auto_error,
        }
    }

    /// Call self as a function.
    fn __call__(&self, request: &Bound<'_, PyAny>) -> Result<String, PyErr> {
        let header = request
            .getattr("headers")
            .ok()
            .and_then(|headers| headers.call_method1("get", ("authorization",)).ok())
            .and_then(|value| value.extract::<String>().ok())
            .filter(|value| !value.is_empty());

        match header {
            Some(header) => Ok(header),
            None if self.auto_error => Err(callable::unauthorized(request.py(), "Digest")),
            None => Ok(String::new()),
        }
    }
}

/// The HTTP authorization credentials in the result of using `HTTPBearer` or
/// `HTTPDigest` in a dependency.
///
/// The HTTP authorization header value is split by the first space.
///
/// The first part is the `scheme`, the second part is the `credentials`.
///
/// For example, in an HTTP Bearer token scheme, the client will send a header
/// like:
///
/// ```
/// Authorization: Bearer deadbeef12346
/// ```
///
/// In this case:
///
/// * `scheme` will have the value `"Bearer"`
/// * `credentials` will have the value `"deadbeef12346"`
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

/// The HTTP Basic credentials given as the result of using `HTTPBasic` in a
/// dependency.
///
/// Read more about it in the
/// [FastAPI docs for HTTP Basic Auth](https://fastapi.tiangolo.com/advanced/security/http-basic-auth/).
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
