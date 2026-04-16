use pyo3::prelude::*;
use pyo3::types::PyDict;

#[pyclass(name = "SessionMiddleware", skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct SessionMiddleware {
    pub secret_key: String,
    pub session_cookie: String,
    pub max_age: Option<i64>,
    pub path: String,
    pub same_site: String,
    pub https_only: bool,
    pub domain: Option<String>,
}

// DO NOT TOUCH THIS PART, because there's no default trait for SessionMiddleware (because secret_key is mandatory)

#[pymethods]
impl SessionMiddleware {
    #[new]
    #[pyo3(signature = (
        secret_key,
        session_cookie="session".to_string(),
        max_age=Some(1209600), // 14 days in seconds
        path="/".to_string(),
        same_site="lax".to_string(),
        https_only=false,
        domain=None
    ))]
    fn new(
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
    // extract mandatory secret_key
    let secret_key: String = match kwargs.get_item("secret_key")? {
        Some(val) if !val.is_none() => val.extract()?,
        _ => {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "SessionMiddleware requires 'secret_key' argument",
            ))
        }
    };

    let mut config = SessionMiddleware {
        secret_key,
        session_cookie: "session".into(),
        max_age: Some(1209600),
        path: "/".into(),
        same_site: "lax".into(),
        https_only: false,
        domain: None,
    };

    set_field!(kwargs, config, "session_cookie", session_cookie: String);
    set_field!(kwargs, config, "max_age", max_age: Option<i64>);
    set_field!(kwargs, config, "path", path: String);
    set_field!(kwargs, config, "same_site", same_site: String);
    set_field!(kwargs, config, "https_only", https_only: bool);
    set_field!(kwargs, config, "domain", domain: Option<String>);

    Ok(config)
}
