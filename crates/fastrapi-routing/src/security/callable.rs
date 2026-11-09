//! The Rust logic for the `__call__` methods of the security classes.
//! The functions read a bearer token, HTTP Basic credentials, or an API key from a request.
//! When the credentials are missing or not valid, they raise an `HTTPException`.

use base64::Engine as _;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};

use crate::security::{HTTPAuthorizationCredentials, HTTPBasicCredentials};
use base64::engine::general_purpose::STANDARD;
use fastrapi_http::exceptions::http_exception;

/// Makes an `HTTPException` error with a status code, a detail, and an optional challenge.
/// RFC 9110 requires a `WWW-Authenticate` header in a 401 response.
fn http_exc(py: Python<'_>, status: u16, detail: &str, challenge: Option<&str>) -> PyErr {
    let headers = match challenge {
        Some(value) => {
            let headers = PyDict::new(py);
            if let Err(err) = headers.set_item(intern!(py, "WWW-Authenticate"), value) {
                return err;
            }
            Some(headers)
        }
        None => None,
    };
    http_exception(py, status, Some(detail), headers)
}

/// Makes a 401 error with the challenge for the expected scheme.
pub(crate) fn unauthorized(py: Python<'_>, challenge: &str) -> PyErr {
    http_exc(py, 401, "Not authenticated", Some(challenge))
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

fn missing<T>(
    request: &Bound<'_, PyAny>,
    auto_error: bool,
    challenge: &str,
) -> PyResult<Option<T>> {
    if auto_error {
        Err(unauthorized(request.py(), challenge))
    } else {
        Ok(None)
    }
}

pub(crate) fn bearer_token(
    request: &Bound<'_, PyAny>,
    auto_error: bool,
) -> PyResult<Option<String>> {
    let header = auth_header(request).unwrap_or_default();
    let (scheme, credentials) = split_authorization(&header);

    if header.is_empty() || !scheme.eq_ignore_ascii_case("bearer") {
        return missing(request, auto_error, "Bearer");
    }
    Ok(Some(credentials.to_owned()))
}

pub(crate) fn raw_authorization(
    request: &Bound<'_, PyAny>,
    auto_error: bool,
) -> PyResult<Option<String>> {
    match auth_header(request).filter(|header| !header.is_empty()) {
        Some(header) => Ok(Some(header)),
        None => missing(request, auto_error, "Bearer"),
    }
}

pub(crate) fn http_digest_call(
    auto_error: bool,
    request: &Bound<'_, PyAny>,
) -> PyResult<Option<HTTPAuthorizationCredentials>> {
    let header = auth_header(request).unwrap_or_default();
    let (scheme, credentials) = split_authorization(&header);
    if scheme.is_empty() || credentials.is_empty() || !scheme.eq_ignore_ascii_case("digest") {
        return missing(request, auto_error, "Digest");
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
    auto_error: bool,
    bearer_format: Option<&str>,
    request: &Bound<'_, PyAny>,
) -> PyResult<Option<HTTPAuthorizationCredentials>> {
    _ = bearer_format;
    let header = auth_header(request).unwrap_or_default();
    let (scheme, credentials) = split_authorization(&header);

    if header.is_empty() || !scheme.eq_ignore_ascii_case("bearer") || credentials.is_empty() {
        return missing(request, auto_error, "Bearer");
    }

    Ok(Some(HTTPAuthorizationCredentials {
        scheme: scheme.to_owned(),
        credentials: credentials.to_owned(),
    }))
}

pub(crate) fn http_basic_call(
    auto_error: bool,
    realm: Option<&str>,
    request: &Bound<'_, PyAny>,
) -> PyResult<Option<HTTPBasicCredentials>> {
    let header = auth_header(request).unwrap_or_default();
    let (scheme, payload) = split_authorization(&header);

    let challenge = match realm {
        Some(realm) => format!("Basic realm=\"{realm}\""),
        None => "Basic".to_owned(),
    };

    let credentials = scheme
        .eq_ignore_ascii_case("basic")
        .then(|| decode_basic(payload))
        .flatten();

    match credentials {
        Some((username, password)) => Ok(Some(HTTPBasicCredentials { username, password })),
        None => missing(request, auto_error, &challenge),
    }
}

pub(crate) fn api_key_call(
    source: &'static str,
    name: &str,
    auto_error: bool,
    request: &Bound<'_, PyAny>,
) -> PyResult<Option<String>> {
    match source_value(request, source, name).filter(|key| !key.is_empty()) {
        Some(key) => Ok(Some(key)),
        None => missing(request, auto_error, "APIKey"),
    }
}
