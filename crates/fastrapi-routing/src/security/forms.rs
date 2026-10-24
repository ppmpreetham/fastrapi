//! The OAuth2 password form classes `OAuth2PasswordRequestForm` and `OAuth2PasswordRequestFormStrict`.
//! These dependency classes read the `username`, the `password`, and the scopes from form data.

use fastrapi_http::exceptions::PyHTTPException;
use fastrapi_http::request::PyRequest;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyString};
use pyo3::{Py, PyErr};

/// This is a dependency class to collect the `username` and `password` as form data
/// for an OAuth2 password flow.
///
/// The OAuth2 specification dictates that for a password flow the data should be
/// collected using form data (instead of JSON) and that it should have the specific
/// fields `username` and `password`.
///
/// All the initialization parameters are extracted from the request.
///
/// Read more about it in the
/// [FastAPI docs for Simple OAuth2 with Password and Bearer](https://fastapi.tiangolo.com/tutorial/security/simple-oauth2/).
///
/// ## Example
///
/// ```python
/// from typing import Annotated
///
/// from fastrapi import Depends, FastAPI
/// from fastrapi.security import OAuth2PasswordRequestForm
///
/// app = FastAPI()
///
///
/// @app.post("/login")
/// def login(form_data: Annotated[OAuth2PasswordRequestForm, Depends()]):
///     data = {}
///     data["scopes"] = []
///     for scope in form_data.scopes:
///         data["scopes"].append(scope)
///     if form_data.client_id:
///         data["client_id"] = form_data.client_id
///     if form_data.client_secret:
///         data["client_secret"] = form_data.client_secret
///     return data
/// ```
///
/// Note that for OAuth2 the scope `items:read` is a single scope in an opaque string.
/// You could have custom internal logic to separate it by colon characters (`:`) or
/// similar, and get the two parts `items` and `read`. Many applications do that to
/// group and organize permissions, you could do it as well in your application, just
/// know that it is application specific, it's not part of the specification.
#[pyclass(
    name = "OAuth2PasswordRequestForm",
    module = "fastrapi.security",
    get_all,
    skip_from_py_object
)]
#[derive(Clone)]
pub struct OAuth2PasswordRequestForm {
    pub grant_type: Option<String>,
    pub username: String,
    pub password: String,
    pub scope: String,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
}

/// This is a dependency class to collect the `username` and `password` as form data
/// for an OAuth2 password flow.
///
/// The OAuth2 specification dictates that for a password flow the data should be
/// collected using form data (instead of JSON) and that it should have the specific
/// fields `username` and `password`.
///
/// All the initialization parameters are extracted from the request.
///
/// The only difference between `OAuth2PasswordRequestFormStrict` and
/// `OAuth2PasswordRequestForm` is that `OAuth2PasswordRequestFormStrict` requires the
/// client to send the form field `grant_type` with the value `"password"`, which
/// is required in the OAuth2 specification (it seems that for no particular reason),
/// while for `OAuth2PasswordRequestForm` `grant_type` is optional.
///
/// Read more about it in the
/// [FastAPI docs for Simple OAuth2 with Password and Bearer](https://fastapi.tiangolo.com/tutorial/security/simple-oauth2/).
///
/// ## Example
///
/// ```python
/// from typing import Annotated
///
/// from fastrapi import Depends, FastAPI
/// from fastrapi.security import OAuth2PasswordRequestForm
///
/// app = FastAPI()
///
///
/// @app.post("/login")
/// def login(form_data: Annotated[OAuth2PasswordRequestFormStrict, Depends()]):
///     data = {}
///     data["scopes"] = []
///     for scope in form_data.scopes:
///         data["scopes"].append(scope)
///     if form_data.client_id:
///         data["client_id"] = form_data.client_id
///     if form_data.client_secret:
///         data["client_secret"] = form_data.client_secret
///     return data
/// ```
///
/// Note that for OAuth2 the scope `items:read` is a single scope in an opaque string.
/// You could have custom internal logic to separate it by colon characters (`:`) or
/// similar, and get the two parts `items` and `read`. Many applications do that to
/// group and organize permissions, you could do it as well in your application, just
/// know that it is application specific, it's not part of the specification.
///
///
/// grant_type: the OAuth2 spec says it is required and MUST be the fixed string "password".
///     This dependency is strict about it. If you want to be permissive, use instead the
///     OAuth2PasswordRequestForm dependency class.
/// username: username string. The OAuth2 spec requires the exact field name "username".
/// password: password string. The OAuth2 spec requires the exact field name "password".
/// scope: Optional string. Several scopes (each one a string) separated by spaces. E.g.
///     "items:read items:write users:read profile openid"
/// client_id: optional string. OAuth2 recommends sending the client_id and client_secret (if any)
///     using HTTP Basic auth, as: client_id:client_secret
/// client_secret: optional string. OAuth2 recommends sending the client_id and client_secret (if any)
///     using HTTP Basic auth, as: client_id:client_secret
#[pyclass(
    name = "OAuth2PasswordRequestFormStrict",
    module = "fastrapi.security",
    get_all,
    skip_from_py_object
)]
#[derive(Clone)]
pub struct OAuth2PasswordRequestFormStrict {
    pub grant_type: String,
    pub username: String,
    pub password: String,
    pub scope: String,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
}

fn form_value(form: &Bound<'_, PyAny>, key: &str) -> PyResult<Option<String>> {
    form.call_method1("get", (key,))?.extract()
}

fn required_form_value(form: &Bound<'_, PyAny>, key: &str) -> PyResult<String> {
    form_value(form, key)?.ok_or_else(|| {
        Python::attach(|py| {
            let exc = PyHTTPException {
                status_code: 422,
                detail: PyString::new(py, &format!("missing form field: {key}"))
                    .into_any()
                    .unbind(),
                headers: None,
            };
            Py::new(py, exc).map(|value| PyErr::from_value(value.into_bound(py).into_any()))
        })
        .unwrap_or_else(|err| err)
    })
}

#[pymethods]
impl OAuth2PasswordRequestForm {
    #[new]
    #[pyo3(signature = (grant_type=None, username=None, password=None, scope=String::new(), client_id=None, client_secret=None))]
    fn new(
        grant_type: Option<String>,
        username: Option<String>,
        password: Option<String>,
        scope: String,
        client_id: Option<String>,
        client_secret: Option<String>,
    ) -> Self {
        Self {
            grant_type,
            username: username.unwrap_or_default(),
            password: password.unwrap_or_default(),
            scope,
            client_id,
            client_secret,
        }
    }

    fn __call__<'py>(&self, _py: Python<'py>, request: &Bound<'py, PyRequest>) -> PyResult<Self> {
        let form = request.call_method0("form_sync")?;
        let form = &form;
        Ok(Self {
            grant_type: form_value(form, "grant_type")?,
            username: required_form_value(form, "username")?,
            password: required_form_value(form, "password")?,
            scope: form_value(form, "scope")?.unwrap_or_default(),
            client_id: form_value(form, "client_id")?,
            client_secret: form_value(form, "client_secret")?,
        })
    }
}

#[pymethods]
impl OAuth2PasswordRequestFormStrict {
    #[new]
    #[pyo3(signature = (grant_type=None, username=None, password=None, scope=String::new(), client_id=None, client_secret=None))]
    fn new(
        grant_type: Option<String>,
        username: Option<String>,
        password: Option<String>,
        scope: String,
        client_id: Option<String>,
        client_secret: Option<String>,
    ) -> PyResult<Self> {
        let grant_type = grant_type.unwrap_or_else(|| "password".to_owned());
        if grant_type != "password" {
            return Err(PyValueError::new_err("grant_type must be password"));
        }
        Ok(Self {
            grant_type,
            username: username.unwrap_or_default(),
            password: password.unwrap_or_default(),
            scope,
            client_id,
            client_secret,
        })
    }

    fn __call__<'py>(&self, _py: Python<'py>, request: &Bound<'py, PyRequest>) -> PyResult<Self> {
        let form = request.call_method0("form_sync")?;
        let form = &form;
        let grant_type = form_value(form, "grant_type")?.unwrap_or_else(|| "password".to_owned());
        if grant_type != "password" {
            return Err(PyValueError::new_err("grant_type must be password"));
        }
        Ok(Self {
            grant_type,
            username: required_form_value(form, "username")?,
            password: required_form_value(form, "password")?,
            scope: form_value(form, "scope")?.unwrap_or_default(),
            client_id: form_value(form, "client_id")?,
            client_secret: form_value(form, "client_secret")?,
        })
    }
}
