//! The Rust logic for the `__call__` methods of the security classes.
//! The functions read a bearer token, HTTP Basic credentials, or an API key from a request.
//! When the credentials are missing or not valid, they raise an `HTTPException`.

use base64::Engine as _;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyType};

use crate::security::{HTTPAuthorizationCredentials, HTTPBasicCredentials};
use base64::engine::general_purpose::STANDARD;
use fastrapi_http::exceptions::http_exception_type;

pub(crate) fn scheme_name(cls: &Bound<'_, PyType>, given: Option<String>) -> PyResult<String> {
    match given.filter(|name| !name.is_empty()) {
        Some(name) => Ok(name),
        None => Ok(cls.name()?.to_string()),
    }
}

pub(crate) fn authenticate_headers<'py>(
    py: Python<'py>,
    challenge: &str,
) -> PyResult<Bound<'py, PyDict>> {
    let headers = PyDict::new(py);
    headers.set_item(intern!(py, "WWW-Authenticate"), challenge)?;
    Ok(headers)
}

pub(crate) fn not_authenticated(headers: Bound<'_, PyAny>) -> PyResult<Bound<'_, PyAny>> {
    http_exception_type(headers.py())?.call1((401, "Not authenticated", headers))
}

pub(crate) fn challenge_error<'py>(
    py: Python<'py>,
    challenge: &str,
) -> PyResult<Bound<'py, PyAny>> {
    not_authenticated(authenticate_headers(py, challenge)?.into_any())
}

fn split_authorization(header: &str) -> (&str, &str) {
    match header.split_once(' ') {
        Some((scheme, credentials)) => (scheme, credentials.trim()),
        None => (header, ""),
    }
}

fn auth_header(request: &Bound<'_, PyAny>) -> Option<String> {
    request
        .getattr("headers")
        .ok()?
        .call_method1("get", ("authorization",))
        .ok()?
        .extract::<String>()
        .ok()
}

fn missing<T>(scheme: &Bound<'_, PyAny>, auto_error: bool) -> PyResult<Option<T>> {
    if !auto_error {
        return Ok(None);
    }
    let error = scheme.call_method0(intern!(scheme.py(), "make_not_authenticated_error"))?;
    Err(PyErr::from_value(error))
}

pub(crate) fn bearer_token(
    scheme_object: &Bound<'_, PyAny>,
    auto_error: bool,
    request: &Bound<'_, PyAny>,
) -> PyResult<Option<String>> {
    let header = auth_header(request).unwrap_or_default();
    let (scheme, credentials) = split_authorization(&header);

    if header.is_empty() || !scheme.eq_ignore_ascii_case("bearer") {
        return missing(scheme_object, auto_error);
    }
    Ok(Some(credentials.to_owned()))
}

pub(crate) fn raw_authorization(
    scheme: &Bound<'_, PyAny>,
    auto_error: bool,
    request: &Bound<'_, PyAny>,
) -> PyResult<Option<String>> {
    match auth_header(request).filter(|header| !header.is_empty()) {
        Some(header) => Ok(Some(header)),
        None => missing(scheme, auto_error),
    }
}

pub(crate) fn http_digest_call(
    scheme_object: &Bound<'_, PyAny>,
    auto_error: bool,
    request: &Bound<'_, PyAny>,
) -> PyResult<Option<HTTPAuthorizationCredentials>> {
    let header = auth_header(request).unwrap_or_default();
    let (scheme, credentials) = split_authorization(&header);
    if scheme.is_empty() || credentials.is_empty() || !scheme.eq_ignore_ascii_case("digest") {
        return missing(scheme_object, auto_error);
    }
    Ok(Some(HTTPAuthorizationCredentials {
        scheme: scheme.to_owned(),
        credentials: credentials.to_owned(),
    }))
}

pub(crate) fn source_value(request: &Bound<'_, PyAny>, source: &str, name: &str) -> Option<String> {
    let container = match source {
        "query" => request.getattr("query_params"),
        "cookie" => request.getattr("cookies"),
        _ => request.getattr("headers"),
    }
    .ok()?;
    container
        .call_method1("get", (name,))
        .ok()?
        .extract::<String>()
        .ok()
}

/// Decodes an RFC 7617 `Basic` credential payload into a `(user, password)` pair.
fn decode_basic(payload: &str) -> Option<(String, String)> {
    let decoded = STANDARD.decode(payload).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let colon = text.find(':')?;
    Some((text[..colon].to_owned(), text[colon + 1..].to_owned()))
}

pub(crate) fn http_bearer_call(
    scheme_object: &Bound<'_, PyAny>,
    auto_error: bool,
    request: &Bound<'_, PyAny>,
) -> PyResult<Option<HTTPAuthorizationCredentials>> {
    let header = auth_header(request).unwrap_or_default();
    let (scheme, credentials) = split_authorization(&header);

    if header.is_empty() || !scheme.eq_ignore_ascii_case("bearer") || credentials.is_empty() {
        return missing(scheme_object, auto_error);
    }

    Ok(Some(HTTPAuthorizationCredentials {
        scheme: scheme.to_owned(),
        credentials: credentials.to_owned(),
    }))
}

pub(crate) fn http_basic_call(
    scheme_object: &Bound<'_, PyAny>,
    auto_error: bool,
    request: &Bound<'_, PyAny>,
) -> PyResult<Option<HTTPBasicCredentials>> {
    let header = auth_header(request).unwrap_or_default();
    let (scheme, payload) = split_authorization(&header);

    let credentials = scheme
        .eq_ignore_ascii_case("basic")
        .then(|| decode_basic(payload))
        .flatten();

    match credentials {
        Some((username, password)) => Ok(Some(HTTPBasicCredentials { username, password })),
        None => missing(scheme_object, auto_error),
    }
}

pub(crate) fn check_api_key(
    scheme: &Bound<'_, PyAny>,
    auto_error: bool,
    api_key: Option<String>,
) -> PyResult<Option<String>> {
    match api_key.filter(|key| !key.is_empty()) {
        Some(key) => Ok(Some(key)),
        None => missing(scheme, auto_error),
    }
}
