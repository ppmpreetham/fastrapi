use pyo3::prelude::*;
use pyo3::types::PyDict;
use smart_default::SmartDefault;

#[pyclass(
    frozen,
    name = "SessionMiddleware",
    get_all,
    from_py_object,
    eq,
    new = "from_fields"
)]
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

pub fn parse_session_params(kwargs: &Bound<'_, PyDict>) -> PyResult<SessionMiddleware> {
    kwargs.extract::<SessionMiddleware>().map_err(Into::into)
}
