use crate::ffi::exceptions::PyHTTPException;
use crate::http::request::PyRequest;
use pyo3::Py;
use pyo3::PyErr;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyAny;
use pyo3::types::PyString;

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
