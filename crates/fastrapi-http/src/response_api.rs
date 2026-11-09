//! This module contains helper functions for the Python response classes.
//! The functions render the content, make the header list, and set cookies.

use cookie::time::Duration;
use std::borrow::Cow;
use cookie::{Cookie, SameSite};
use fastrapi_pyutil::imports::SIMPLE_COOKIE;
use pyo3::exceptions::{PyAssertionError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use pyo3::{Borrowed, intern};

pub struct HeadersArg(pub Py<PyDict>);

impl FromPyObject<'_, '_> for HeadersArg {
    type Error = PyErr;

    fn extract(obj: Borrowed<'_, '_, PyAny>) -> PyResult<Self> {
        if let Ok(dict) = obj.cast::<PyDict>() {
            return Ok(Self(dict.to_owned().unbind()));
        }
        let py = obj.py();
        let dict = PyDict::new(py);
        for item in obj.call_method0(intern!(py, "items"))?.try_iter()? {
            let (key, value): (Bound<'_, PyAny>, Bound<'_, PyAny>) = item?.extract()?;
            dict.set_item(key, value)?;
        }
        Ok(Self(dict.unbind()))
    }
}

fastrapi_pyutil::cached_py_import!(JSON_DUMPS, "json", "dumps");
fastrapi_pyutil::cached_py_import!(
    MUTABLE_HEADERS,
    "starlette.datastructures",
    "MutableHeaders"
);

pub struct ResponseParts {
    pub content: Py<PyAny>,
    pub status_code: u16,
    pub headers: Option<Py<PyDict>>,
    pub media_type: Option<String>,
    pub background: Option<Py<PyAny>>,
    pub raw_headers: Py<PyList>,
    pub body: Py<PyAny>,
}

pub fn response_parts(
    py: Python<'_>,
    class_name: &str,
    content: Option<Py<PyAny>>,
    status_code: u16,
    headers: Option<Py<PyDict>>,
    media_type: Option<String>,
    background: Option<Py<PyAny>>,
) -> PyResult<ResponseParts> {
    let content = content.unwrap_or_else(|| py.None());
    let media_type = media_type.or_else(|| {
        match class_name {
            "HTMLResponse" => Some("text/html"),
            "PlainTextResponse" => Some("text/plain"),
            "JSONResponse" | "ORJSONResponse" | "UJSONResponse" => Some("application/json"),
            _ => None,
        }
        .map(str::to_owned)
    });
    let streaming = class_name == "StreamingResponse";
    let body = if streaming {
        py.None()
    } else {
        render(py, content.bind(py), class_name, "utf-8")?.unbind()
    };
    let raw_headers = init_headers(
        py,
        headers.as_ref().map(|headers| headers.bind(py).as_any()),
        (!streaming).then(|| body.bind(py)),
        status_code,
        media_type.as_deref(),
        "utf-8",
    )?;
    Ok(ResponseParts {
        content,
        status_code,
        headers,
        media_type,
        background,
        raw_headers,
        body,
    })
}

pub fn render<'py>(
    py: Python<'py>,
    content: &Bound<'py, PyAny>,
    kind: &str,
    charset: &str,
) -> PyResult<Bound<'py, PyAny>> {
    if matches!(kind, "JSONResponse" | "ORJSONResponse" | "UJSONResponse") {
        if charset.eq_ignore_ascii_case("utf-8")
            && let Some(rendered) = fastrapi_pyutil::json::plain_json_bytes(content)?
        {
            return Ok(rendered.into_any());
        }
        let kwargs = PyDict::new(py);
        kwargs.set_item(pyo3::intern!(py, "ensure_ascii"), false)?;
        kwargs.set_item(pyo3::intern!(py, "allow_nan"), false)?;
        kwargs.set_item(pyo3::intern!(py, "separators"), (",", ":"))?;
        return JSON_DUMPS
            .get(py)?
            .call((content,), Some(&kwargs))?
            .call_method1(pyo3::intern!(py, "encode"), (charset,));
    }
    if content.is_none() {
        return Ok(PyBytes::new(py, b"").into_any());
    }
    if content.is_instance_of::<PyBytes>() || content.get_type().name()?.to_str()? == "memoryview" {
        return Ok(content.clone());
    }
    content.call_method1(pyo3::intern!(py, "encode"), (charset,))
}

pub fn headers<'py>(py: Python<'py>, raw: &Py<PyList>) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item(pyo3::intern!(py, "raw"), raw.bind(py))?;
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
        for item in supplied
            .call_method0(pyo3::intern!(py, "items"))?
            .try_iter()?
        {
            let item = item?;
            let name = item
                .get_item(0)?
                .call_method0(pyo3::intern!(py, "lower"))?
                .call_method1(pyo3::intern!(py, "encode"), ("latin-1",))?;
            let value = item
                .get_item(1)?
                .call_method1(pyo3::intern!(py, "encode"), ("latin-1",))?;
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
        headers.set_item("content-type", content_type(media_type, charset).as_ref())?;
    }
    Ok(raw)
}

#[must_use]
pub fn content_type<'a>(media_type: &'a str, charset: &str) -> Cow<'a, str> {
    let has_charset = media_type
        .as_bytes()
        .windows(8)
        .any(|window| window.eq_ignore_ascii_case(b"charset="));
    if !media_type.starts_with("text/") || has_charset {
        return Cow::Borrowed(media_type);
    }
    if charset == "utf-8" {
        match media_type {
            "text/plain" => return Cow::Borrowed("text/plain; charset=utf-8"),
            "text/html" => return Cow::Borrowed("text/html; charset=utf-8"),
            "text/event-stream" => return Cow::Borrowed("text/event-stream; charset=utf-8"),
            _ => {}
        }
    }
    Cow::Owned(format!("{media_type}; charset={charset}"))
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
        morsel.set_item(pyo3::intern!(py, "path"), value)?;
    }
    if let Some(value) = cookie.domain() {
        morsel.set_item(pyo3::intern!(py, "domain"), value)?;
    }
    if let Some(value) = cookie.max_age() {
        morsel.set_item("max-age", value.whole_seconds())?;
    }
    if secure {
        morsel.set_item(pyo3::intern!(py, "secure"), true)?;
    }
    if httponly {
        morsel.set_item(pyo3::intern!(py, "httponly"), true)?;
    }
    if let Some(value) = samesite {
        morsel.set_item(pyo3::intern!(py, "samesite"), value)?;
    }
    if partitioned {
        morsel.set_item(pyo3::intern!(py, "partitioned"), true)?;
    }
    if let Some(value) = expires {
        if value.is_instance(
            &py.import("datetime")?
                .getattr(pyo3::intern!(py, "datetime"))?,
        )? {
            let kwargs = PyDict::new(py);
            kwargs.set_item(pyo3::intern!(py, "usegmt"), true)?;
            morsel.set_item(
                "expires",
                py.import("email.utils")?
                    .getattr(pyo3::intern!(py, "format_datetime"))?
                    .call((value,), Some(&kwargs))?,
            )?;
        } else {
            morsel.set_item(pyo3::intern!(py, "expires"), value)?;
        }
    }
    let encoded = morsel
        .call_method0(pyo3::intern!(py, "OutputString"))?
        .call_method1(pyo3::intern!(py, "encode"), ("latin-1",))?;
    raw.bind(py)
        .append((PyBytes::new(py, b"set-cookie"), encoded))
}
