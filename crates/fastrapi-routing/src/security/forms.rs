//! The OAuth2 password form classes `OAuth2PasswordRequestForm` and `OAuth2PasswordRequestFormStrict`.
//! These dependency classes read the `username`, the `password`, and the scopes from form data.

use fastrapi_http::exceptions::request_validation_error_type;
use fastrapi_http::request::PyRequest;
use pyo3::prelude::*;
use pyo3::types::{IntoPyDict, PyDict, PyList};

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
    subclass,
    skip_from_py_object
)]
#[derive(Clone)]
pub struct OAuth2PasswordRequestForm {
    #[pyo3(get)]
    pub grant_type: Option<String>,
    #[pyo3(get)]
    pub username: String,
    #[pyo3(get)]
    pub password: String,
    pub scope: String,
    #[pyo3(get)]
    pub client_id: Option<String>,
    #[pyo3(get)]
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
    extends = OAuth2PasswordRequestForm,
    subclass
)]
pub struct OAuth2PasswordRequestFormStrict;

struct PasswordForm {
    grant_type: Option<String>,
    username: String,
    password: String,
    scope: String,
    client_id: Option<String>,
    client_secret: Option<String>,
}

fn form_error<'py>(
    py: Python<'py>,
    kind: &str,
    field: &str,
    msg: &str,
    input: Option<&str>,
) -> PyResult<Bound<'py, PyDict>> {
    let error = PyDict::new(py);
    error.set_item("type", kind)?;
    error.set_item("loc", ("body", field))?;
    error.set_item("msg", msg)?;
    error.set_item("input", input)?;
    if input.is_some() {
        error.set_item("ctx", [("pattern", GRANT_TYPE_PATTERN)].into_py_dict(py)?)?;
    }
    Ok(error)
}

const GRANT_TYPE_PATTERN: &str = "^password$";

fn password_form(request: &Bound<'_, PyRequest>, strict: bool) -> PyResult<PasswordForm> {
    let py = request.py();
    let form = request.call_method0("form_sync")?;
    let field = |key: &str| -> PyResult<Option<String>> {
        Ok(form
            .call_method1("get", (key,))?
            .extract::<Option<String>>()?
            .filter(|value| !value.is_empty()))
    };
    let errors = PyList::empty(py);
    let grant_type = field("grant_type")?;
    match grant_type.as_deref() {
        Some("password") => {}
        Some(other) => errors.append(form_error(
            py,
            "string_pattern_mismatch",
            "grant_type",
            "String should match pattern '^password$'",
            Some(other),
        )?)?,
        None if strict => {
            errors.append(form_error(
                py,
                "missing",
                "grant_type",
                "Field required",
                None,
            )?)?;
        }
        None => {}
    }
    let required = |key: &str| -> PyResult<String> {
        let value = field(key)?;
        if value.is_none() {
            errors.append(form_error(py, "missing", key, "Field required", None)?)?;
        }
        Ok(value.unwrap_or_default())
    };
    let username = required("username")?;
    let password = required("password")?;
    if !errors.is_empty() {
        return Err(PyErr::from_value(
            request_validation_error_type(py)?.call1((errors,))?,
        ));
    }
    Ok(PasswordForm {
        grant_type,
        username,
        password,
        scope: field("scope")?.unwrap_or_default(),
        client_id: field("client_id")?,
        client_secret: field("client_secret")?,
    })
}

#[pymethods]
impl OAuth2PasswordRequestForm {
    #[new]
    #[pyo3(signature = (*, grant_type=None, username, password, scope=String::new(), client_id=None, client_secret=None))]
    const fn new(
        grant_type: Option<String>,
        username: String,
        password: String,
        scope: String,
        client_id: Option<String>,
        client_secret: Option<String>,
    ) -> Self {
        Self {
            grant_type,
            username,
            password,
            scope,
            client_id,
            client_secret,
        }
    }

    #[getter]
    fn scopes(&self) -> Vec<&str> {
        self.scope.split_whitespace().collect()
    }

    fn __call__(&self, request: &Bound<'_, PyRequest>) -> PyResult<Self> {
        password_form(request, false).map(Self::from)
    }
}

impl From<PasswordForm> for OAuth2PasswordRequestForm {
    fn from(form: PasswordForm) -> Self {
        Self {
            grant_type: form.grant_type,
            username: form.username,
            password: form.password,
            scope: form.scope,
            client_id: form.client_id,
            client_secret: form.client_secret,
        }
    }
}

#[pymethods]
impl OAuth2PasswordRequestFormStrict {
    #[new]
    #[pyo3(signature = (*, grant_type, username, password, scope=String::new(), client_id=None, client_secret=None))]
    fn new(
        grant_type: String,
        username: String,
        password: String,
        scope: String,
        client_id: Option<String>,
        client_secret: Option<String>,
    ) -> PyClassInitializer<Self> {
        PyClassInitializer::from(OAuth2PasswordRequestForm::new(
            Some(grant_type),
            username,
            password,
            scope,
            client_id,
            client_secret,
        ))
        .add_subclass(Self)
    }

    fn __call__(&self, request: &Bound<'_, PyRequest>) -> PyResult<Py<Self>> {
        let form = OAuth2PasswordRequestForm::from(password_form(request, true)?);
        Py::new(
            request.py(),
            PyClassInitializer::from(form).add_subclass(Self),
        )
    }
}
