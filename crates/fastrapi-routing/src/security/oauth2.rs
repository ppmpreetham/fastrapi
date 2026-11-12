//! The OAuth2 and OpenID Connect security classes.
//! These classes read a bearer token from the `Authorization` header.
//! They also hold the OAuth2 flows for the OpenAPI schema.

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyType};
use smart_default::SmartDefault;

use super::{callable, scheme};

/// This is the base class for OAuth2 authentication, an instance of it would be used
/// as a dependency. All other OAuth2 classes inherit from it and customize it for
/// each OAuth2 flow.
///
/// You normally would not create a new class inheriting from it but use one of the
/// existing subclasses, and maybe compose them if you want to support multiple flows.
///
/// Read more about it in the
/// [FastAPI docs for Security](https://fastapi.tiangolo.com/tutorial/security/).
#[pyclass(
    frozen,
    subclass,
    name = "OAuth2",
    module = "fastrapi.security",
    get_all,
    from_py_object
)]
#[derive(Clone, Debug)]
pub struct OAuth2 {
    pub flows: Py<PyAny>,
    pub scheme_name: Option<String>,
    pub description: Option<String>,
    pub auto_error: bool,
}

#[pymethods]
impl OAuth2 {
    #[new]
    #[classmethod]
    #[pyo3(signature = (flows=None, scheme_name=None, description=None, auto_error=true))]
    fn new(
        cls: &Bound<'_, PyType>,
        flows: Option<Py<PyAny>>,
        scheme_name: Option<String>,
        description: Option<String>,
        auto_error: bool,
    ) -> PyResult<Self> {
        Ok(Self {
            flows: flows.unwrap_or_else(|| PyDict::new(cls.py()).into()),
            scheme_name: Some(callable::scheme_name(cls, scheme_name)?),
            description,
            auto_error,
        })
    }

    /// Call self as a function.
    fn __call__(slf: &Bound<'_, Self>, request: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
        callable::raw_authorization(slf.as_any(), slf.get().auto_error, request)
    }

    #[getter]
    fn model<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        scheme::oauth2_model(self.flows.bind(py), self.description.as_deref())
    }
}

/// OAuth2 flow for authentication using a bearer token obtained with a password.
/// An instance of it would be used as a dependency.
///
/// Read more about it in the
/// [FastAPI docs for Simple OAuth2 with Password and Bearer](https://fastapi.tiangolo.com/tutorial/security/simple-oauth2/).
#[pyclass(
    frozen,
    subclass,
    name = "OAuth2PasswordBearer",
    module = "fastrapi.security",
    get_all,
    from_py_object
)]
#[derive(SmartDefault, Clone, Debug)]
pub struct OAuth2PasswordBearer {
    pub token_url: String,
    pub refresh_url: Option<String>,
    pub scheme_name: Option<String>,
    pub scopes: Option<Py<PyDict>>,
    pub description: Option<String>,
    #[default(true)]
    pub auto_error: bool,
}

#[pymethods]
impl OAuth2PasswordBearer {
    #[new]
    #[classmethod]
    #[pyo3(signature = (
        tokenUrl=None,
        scheme_name=None,
        scopes=None,
        description=None,
        auto_error=true,
        refreshUrl=None,
        *,
        token_url=None,
        refresh_url=None
    ))]
    #[expect(
        non_snake_case,
        reason = "FastAPI names these arguments tokenUrl and refreshUrl"
    )]
    #[expect(
        clippy::too_many_arguments,
        reason = "the signature copies FastAPI and keeps the snake_case names"
    )]
    fn new(
        cls: &Bound<'_, PyType>,
        tokenUrl: Option<String>,
        scheme_name: Option<String>,
        scopes: Option<Py<PyDict>>,
        description: Option<String>,
        auto_error: bool,
        refreshUrl: Option<String>,
        token_url: Option<String>,
        refresh_url: Option<String>,
    ) -> PyResult<Self> {
        Ok(Self {
            token_url: required_url(tokenUrl, token_url, "tokenUrl")?,
            refresh_url: refreshUrl.or(refresh_url),
            scheme_name: Some(callable::scheme_name(cls, scheme_name)?),
            scopes,
            description,
            auto_error,
        })
    }

    /// Call self as a function.
    fn __call__(slf: &Bound<'_, Self>, request: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
        callable::bearer_token(slf.as_any(), slf.get().auto_error, request)
    }

    #[getter]
    fn model<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let flows = scheme::oauth2_flow(
            py,
            "password",
            &[
                ("tokenUrl", Some(&self.token_url)),
                ("refreshUrl", self.refresh_url.as_deref()),
            ],
            self.scopes.as_ref(),
        )?;
        scheme::oauth2_model(&flows, self.description.as_deref())
    }
}

/// OAuth2 flow for authentication using a bearer token obtained with an OAuth2 code
/// flow. An instance of it would be used as a dependency.
#[pyclass(
    frozen,
    subclass,
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
    #[classmethod]
    #[pyo3(signature = (
        authorizationUrl=None,
        tokenUrl=None,
        refreshUrl=None,
        scheme_name=None,
        scopes=None,
        description=None,
        auto_error=true,
        *,
        authorization_url=None,
        token_url=None,
        refresh_url=None
    ))]
    #[expect(
        non_snake_case,
        reason = "FastAPI names these arguments authorizationUrl, tokenUrl and refreshUrl"
    )]
    #[expect(
        clippy::too_many_arguments,
        reason = "the signature copies FastAPI and keeps the snake_case names"
    )]
    fn new(
        cls: &Bound<'_, PyType>,
        authorizationUrl: Option<String>,
        tokenUrl: Option<String>,
        refreshUrl: Option<String>,
        scheme_name: Option<String>,
        scopes: Option<Py<PyDict>>,
        description: Option<String>,
        auto_error: bool,
        authorization_url: Option<String>,
        token_url: Option<String>,
        refresh_url: Option<String>,
    ) -> PyResult<Self> {
        Ok(Self {
            authorization_url: required_url(
                authorizationUrl,
                authorization_url,
                "authorizationUrl",
            )?,
            token_url: required_url(tokenUrl, token_url, "tokenUrl")?,
            refresh_url: refreshUrl.or(refresh_url),
            scopes,
            scheme_name: Some(callable::scheme_name(cls, scheme_name)?),
            description,
            auto_error,
        })
    }

    /// Call self as a function.
    fn __call__(slf: &Bound<'_, Self>, request: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
        callable::bearer_token(slf.as_any(), slf.get().auto_error, request)
    }

    #[getter]
    fn model<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let flows = scheme::oauth2_flow(
            py,
            "authorizationCode",
            &[
                ("authorizationUrl", Some(&self.authorization_url)),
                ("tokenUrl", Some(&self.token_url)),
                ("refreshUrl", self.refresh_url.as_deref()),
            ],
            self.scopes.as_ref(),
        )?;
        scheme::oauth2_model(&flows, self.description.as_deref())
    }
}

fn required_url(camel: Option<String>, snake: Option<String>, name: &str) -> PyResult<String> {
    camel
        .or(snake)
        .ok_or_else(|| PyTypeError::new_err(format!("missing required argument: '{name}'")))
}

/// OpenID Connect authentication class. An instance of it would be used as a
/// dependency.
///
/// **Warning**: this is only a stub to connect the components with OpenAPI in FastAPI,
/// but it doesn't implement the full OpenIdConnect scheme, for example, it doesn't use
/// the OpenIDConnect URL. You would need to subclass it and implement it in your
/// code.
#[pyclass(
    frozen,
    subclass,
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
    #[classmethod]
    #[pyo3(signature = (
        openIdConnectUrl=None,
        scheme_name=None,
        description=None,
        auto_error=true,
        *,
        openid_connect_url=None
    ))]
    #[expect(
        non_snake_case,
        reason = "FastAPI names this argument openIdConnectUrl"
    )]
    fn new(
        cls: &Bound<'_, PyType>,
        openIdConnectUrl: Option<String>,
        scheme_name: Option<String>,
        description: Option<String>,
        auto_error: bool,
        openid_connect_url: Option<String>,
    ) -> PyResult<Self> {
        Ok(Self {
            openid_connect_url: required_url(
                openIdConnectUrl,
                openid_connect_url,
                "openIdConnectUrl",
            )?,
            scheme_name: Some(callable::scheme_name(cls, scheme_name)?),
            description,
            auto_error,
        })
    }

    /// Call self as a function.
    fn __call__(slf: &Bound<'_, Self>, request: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
        callable::raw_authorization(slf.as_any(), slf.get().auto_error, request)
    }

    #[getter]
    fn model<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        scheme::openid_model(py, &self.openid_connect_url, self.description.as_deref())
    }
}

macro_rules! bearer_challenge {
    ($($ty:ty),+) => {$(
        #[pymethods]
        impl $ty {
            fn make_not_authenticated_error<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
                callable::challenge_error(py, "Bearer")
            }
        }
    )+};
}

bearer_challenge!(
    OAuth2,
    OAuth2PasswordBearer,
    OAuth2AuthorizationCodeBearer,
    OpenIdConnect
);
