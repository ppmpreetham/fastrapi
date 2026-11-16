//! The HTTP security classes `HTTPBearer`, `HTTPBasic`, and `HTTPDigest`, and their credential classes.
//! Each security class reads the `Authorization` header and gives the credentials to the endpoint.

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyType};
use smart_default::SmartDefault;
use std::borrow::Cow;

use super::{callable, scheme};

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
    subclass,
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
    #[classmethod]
    #[pyo3(signature = (*, bearerFormat=None, scheme_name=None, description=None, auto_error=true, bearer_format=None))]
    #[expect(non_snake_case, reason = "FastAPI names this argument bearerFormat")]
    fn new(
        cls: &Bound<'_, PyType>,
        bearerFormat: Option<String>,
        scheme_name: Option<String>,
        description: Option<String>,
        auto_error: bool,
        bearer_format: Option<String>,
    ) -> PyResult<Self> {
        Ok(Self {
            bearer_format: bearerFormat.or(bearer_format),
            scheme_name: Some(callable::scheme_name(cls, scheme_name)?),
            description,
            auto_error,
        })
    }

    #[pyo3(name = "_resolve")]
    fn resolve(
        slf: &Bound<'_, Self>,
        request: &Bound<'_, PyAny>,
    ) -> PyResult<Option<super::HTTPAuthorizationCredentials>> {
        callable::http_bearer_call(slf.as_any(), slf.get().auto_error, request)
    }

    #[getter]
    fn model<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        scheme::bearer_model(
            py,
            self.bearer_format.as_deref(),
            self.description.as_deref(),
        )
    }
}

impl HTTPBearer {
    const fn challenge(&self) -> Cow<'static, str> {
        Cow::Borrowed("Bearer")
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
    subclass,
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
    #[classmethod]
    #[pyo3(signature = (*, scheme_name=None, realm=None, description=None, auto_error=true))]
    fn new(
        cls: &Bound<'_, PyType>,
        scheme_name: Option<String>,
        realm: Option<String>,
        description: Option<String>,
        auto_error: bool,
    ) -> PyResult<Self> {
        Ok(Self {
            scheme_name: Some(callable::scheme_name(cls, scheme_name)?),
            realm,
            description,
            auto_error,
        })
    }

    #[pyo3(name = "_resolve")]
    fn resolve(
        slf: &Bound<'_, Self>,
        request: &Bound<'_, PyAny>,
    ) -> PyResult<Option<super::HTTPBasicCredentials>> {
        callable::http_basic_call(slf.as_any(), slf.get().auto_error, request)
    }

    #[getter]
    fn model<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        scheme::http_model(py, "basic", self.description.as_deref())
    }
}

impl HTTPBasic {
    fn challenge(&self) -> Cow<'static, str> {
        match &self.realm {
            Some(realm) if !realm.is_empty() => Cow::Owned(format!("Basic realm=\"{realm}\"")),
            _ => Cow::Borrowed("Basic"),
        }
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
    subclass,
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
    #[classmethod]
    #[pyo3(signature = (*, scheme_name=None, description=None, auto_error=true))]
    fn new(
        cls: &Bound<'_, PyType>,
        scheme_name: Option<String>,
        description: Option<String>,
        auto_error: bool,
    ) -> PyResult<Self> {
        Ok(Self {
            scheme_name: Some(callable::scheme_name(cls, scheme_name)?),
            description,
            auto_error,
        })
    }

    #[pyo3(name = "_resolve")]
    fn resolve(
        slf: &Bound<'_, Self>,
        request: &Bound<'_, PyAny>,
    ) -> PyResult<Option<HTTPAuthorizationCredentials>> {
        callable::http_digest_call(slf.as_any(), slf.get().auto_error, request)
    }

    #[getter]
    fn model<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        scheme::http_model(py, "digest", self.description.as_deref())
    }
}

impl HTTPDigest {
    const fn challenge(&self) -> Cow<'static, str> {
        Cow::Borrowed("Digest")
    }
}

macro_rules! http_challenge {
    ($($ty:ty),+) => {$(
        #[pymethods]
        impl $ty {
            fn make_authenticate_headers<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
                callable::authenticate_headers(py, &self.challenge())
            }

            fn make_not_authenticated_error<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
                callable::not_authenticated(slf.call_method0(intern!(slf.py(), "make_authenticate_headers"))?)
            }
        }
    )+};
}

http_challenge!(HTTPBearer, HTTPBasic, HTTPDigest);

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

#[pymethods]
impl HTTPAuthorizationCredentials {
    #[new]
    #[pyo3(signature = (*, scheme, credentials))]
    const fn new(scheme: String, credentials: String) -> Self {
        Self {
            scheme,
            credentials,
        }
    }
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

#[pymethods]
impl HTTPBasicCredentials {
    #[new]
    #[pyo3(signature = (*, username, password))]
    const fn new(username: String, password: String) -> Self {
        Self { username, password }
    }
}

fn credentials_dump<'py>(
    py: Python<'py>,
    fields: [(&str, &str); 2],
    include: Option<&Bound<'py, PyAny>>,
    exclude: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyDict>> {
    let dump = PyDict::new(py);
    for (name, value) in fields {
        let included = include.map_or(Ok(true), |include| include.contains(name))?;
        let excluded = exclude.map_or(Ok(false), |exclude| exclude.contains(name))?;
        if included && !excluded {
            dump.set_item(name, value)?;
        }
    }
    Ok(dump)
}

fn credentials_json(fields: [(&str, &str); 2]) -> String {
    let object: serde_json::Map<String, serde_json::Value> = fields
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.into()))
        .collect();
    serde_json::Value::Object(object).to_string()
}

macro_rules! credentials_model {
    ($ty:ident, $first:ident, $second:ident) => {
        impl $ty {
            const fn fields(&self) -> [(&'static str, &str); 2] {
                [
                    (stringify!($first), self.$first.as_str()),
                    (stringify!($second), self.$second.as_str()),
                ]
            }
        }

        #[pymethods]
        impl $ty {
            #[pyo3(signature = (*, include=None, exclude=None, **_options))]
            fn model_dump<'py>(
                &self,
                py: Python<'py>,
                include: Option<&Bound<'py, PyAny>>,
                exclude: Option<&Bound<'py, PyAny>>,
                _options: Option<&Bound<'py, PyDict>>,
            ) -> PyResult<Bound<'py, PyDict>> {
                credentials_dump(py, self.fields(), include, exclude)
            }

            #[pyo3(signature = (**_options))]
            fn model_dump_json(&self, _options: Option<&Bound<'_, PyDict>>) -> String {
                credentials_json(self.fields())
            }

            #[classmethod]
            fn model_validate<'py>(
                cls: &Bound<'py, PyType>,
                value: &Bound<'py, PyAny>,
            ) -> PyResult<Bound<'py, PyAny>> {
                if value.is_instance(cls)? {
                    return Ok(value.clone());
                }
                cls.call((), Some(value.cast::<PyDict>()?))
            }

            fn model_copy(&self) -> Self {
                self.clone()
            }
        }
    };
}

credentials_model!(HTTPAuthorizationCredentials, scheme, credentials);
credentials_model!(HTTPBasicCredentials, username, password);
