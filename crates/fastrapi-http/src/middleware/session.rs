//! This module contains the `SessionMiddleware` class.
//! It reads the session cookie configuration from Python keyword arguments.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use smart_default::SmartDefault;

#[pyclass(frozen, name = "SessionMiddleware", get_all, from_py_object, eq)]
#[derive(SmartDefault, Clone, Debug, PartialEq, Eq)]
pub struct SessionMiddleware {
    pub secret_key: String,

    #[default("session".to_string())]
    pub session_cookie: String,

    #[default(Some(1209600))]
    pub max_age: Option<i64>,

    #[default("/".to_string())]
    pub path: String,

    #[default("lax".to_string())]
    pub same_site: String,

    #[default(false)]
    pub https_only: bool,

    #[default(None)]
    pub domain: Option<String>,
}

#[pymethods]
impl SessionMiddleware {
    #[new]
    #[pyo3(signature = (
        *,
        secret_key,
        session_cookie="session".to_string(),
        max_age=Some(1_209_600),
        path="/".to_string(),
        same_site="lax".to_string(),
        https_only=false,
        domain=None
    ))]
    const fn new(
        secret_key: String,
        session_cookie: String,
        max_age: Option<i64>,
        path: String,
        same_site: String,
        https_only: bool,
        domain: Option<String>,
    ) -> Self {
        Self {
            secret_key,
            session_cookie,
            max_age,
            path,
            same_site,
            https_only,
            domain,
        }
    }
}

pub fn parse_session_params(kwargs: &Bound<'_, PyDict>) -> PyResult<SessionMiddleware> {
    kwargs
        .py()
        .get_type::<SessionMiddleware>()
        .call((), Some(kwargs))?
        .extract()
        .map_err(Into::into)
}
