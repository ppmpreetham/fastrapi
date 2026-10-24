use cookie::time::Duration;
use cookie::{Cookie, SameSite};
use fastrapi_pyutil::imports::SIMPLE_COOKIE;
use pyo3::exceptions::{PyAssertionError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};

fastrapi_pyutil::cached_py_import!(JSON_DUMPS, "json", "dumps");
fastrapi_pyutil::cached_py_import!(
    MUTABLE_HEADERS,
    "starlette.datastructures",
    "MutableHeaders"
);

pub fn render<'py>(
    py: Python<'py>,
    content: &Bound<'py, PyAny>,
    kind: &str,
    charset: &str,
) -> PyResult<Bound<'py, PyAny>> {
    if matches!(kind, "JSONResponse" | "ORJSONResponse" | "UJSONResponse") {
        let kwargs = PyDict::new(py);
        kwargs.set_item("ensure_ascii", false)?;
        kwargs.set_item("allow_nan", false)?;
        kwargs.set_item("separators", (",", ":"))?;
        return JSON_DUMPS
            .get(py)?
            .call((content,), Some(&kwargs))?
            .call_method1("encode", (charset,));
    }
    if content.is_none() {
        return Ok(PyBytes::new(py, b"").into_any());
    }
    if content.is_instance_of::<PyBytes>() || content.get_type().name()?.to_str()? == "memoryview" {
        return Ok(content.clone());
    }
    content.call_method1("encode", (charset,))
}

pub fn headers<'py>(py: Python<'py>, raw: &Py<PyList>) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("raw", raw.bind(py))?;
    MUTABLE_HEADERS.get(py)?.call((), Some(&kwargs))
}

pub fn init_headers(
    py: Python<'_>,
    supplied: Option<&Bound<'_, PyAny>>,
    body: Option<&Bound<'_, PyAny>>,
    status: u16,
    media_type: Option<&str>,
    charset: &str,
) -> PyResult<Py<PyList>> {
    let raw = PyList::empty(py);
    if let Some(supplied) = supplied {
        for item in supplied.call_method0("items")?.try_iter()? {
            let item = item?;
            let name = item
                .get_item(0)?
                .call_method0("lower")?
                .call_method1("encode", ("latin-1",))?;
            let value = item.get_item(1)?.call_method1("encode", ("latin-1",))?;
            raw.append((name, value))?;
        }
    }
    let raw = raw.unbind();
    let headers = headers(py, &raw)?;
    if status >= 200
        && status != 204
        && status != 304
        && !headers.contains("content-length")?
        && let Some(body) = body
    {
        headers.set_item("content-length", body.len()?.to_string())?;
    }
    if let Some(media_type) = media_type
        && !headers.contains("content-type")?
    {
        let content_type =
            if media_type.starts_with("text/") && !media_type.to_lowercase().contains("charset=") {
                format!("{media_type}; charset={charset}")
            } else {
                media_type.to_owned()
            };
        headers.set_item("content-type", content_type)?;
    }
    Ok(raw)
}

pub fn set_cookie(
    py: Python<'_>,
    raw: &Py<PyList>,
    key: String,
    value: String,
    max_age: Option<i64>,
    expires: Option<&Bound<'_, PyAny>>,
    path: Option<String>,
    domain: Option<String>,
    secure: bool,
    httponly: bool,
    samesite: Option<String>,
    partitioned: bool,
) -> PyResult<()> {
    let same_site = match samesite.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("lax") => Some(SameSite::Lax),
        Some("strict") => Some(SameSite::Strict),
        Some("none") => Some(SameSite::None),
        None => None,
        _ => {
            return Err(PyAssertionError::new_err(
                "samesite must be either 'strict', 'lax' or 'none'",
            ));
        }
    };
    if partitioned && py.version_info() < (3, 14) {
        return Err(PyValueError::new_err(
            "Partitioned cookies are only supported in Python 3.14 and above.",
        ));
    }
    let mut cookie = Cookie::new(key, value);
    cookie.set_secure(secure);
    cookie.set_http_only(httponly);
    if let Some(value) = path {
        cookie.set_path(value);
    }
    if let Some(value) = domain {
        cookie.set_domain(value);
    }
    if let Some(value) = same_site {
        cookie.set_same_site(value);
    }
    if let Some(value) = max_age {
        cookie.set_max_age(Duration::seconds(value));
    }
    let jar = SIMPLE_COOKIE.get(py)?.call0()?;
    jar.set_item(cookie.name(), cookie.value())?;
    let morsel = jar.get_item(cookie.name())?;
    if let Some(value) = cookie.path() {
        morsel.set_item("path", value)?;
    }
    if let Some(value) = cookie.domain() {
        morsel.set_item("domain", value)?;
    }
    if let Some(value) = cookie.max_age() {
        morsel.set_item("max-age", value.whole_seconds())?;
    }
    if secure {
        morsel.set_item("secure", true)?;
    }
    if httponly {
        morsel.set_item("httponly", true)?;
    }
    if let Some(value) = samesite {
        morsel.set_item("samesite", value)?;
    }
    if partitioned {
        morsel.set_item("partitioned", true)?;
    }
    if let Some(value) = expires {
        if value.is_instance(&py.import("datetime")?.getattr("datetime")?)? {
            let kwargs = PyDict::new(py);
            kwargs.set_item("usegmt", true)?;
            morsel.set_item(
                "expires",
                py.import("email.utils")?
                    .getattr("format_datetime")?
                    .call((value,), Some(&kwargs))?,
            )?;
        } else {
            morsel.set_item("expires", value)?;
        }
    }
    let encoded = morsel
        .call_method0("OutputString")?
        .call_method1("encode", ("latin-1",))?;
    raw.bind(py)
        .append((PyBytes::new(py, b"set-cookie"), encoded))
}
