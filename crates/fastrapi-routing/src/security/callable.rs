//! The Rust logic for the `__call__` methods of the security classes.
//! The functions read a bearer token, HTTP Basic credentials, or an API key from a request.
//! When the credentials are missing or not valid, they raise an `HTTPException`.

use base64::Engine as _;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyType};

use crate::security::{HTTPAuthorizationCredentials, HTTPBasicCredentials};
use base64::alphabet;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::GeneralPurpose;
use base64::engine::general_purpose::GeneralPurposeConfig;
use fastrapi_core::params::ParameterSource;
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
    let py = request.py();
    request
        .getattr(intern!(py, "headers"))
        .ok()?
        .call_method1(intern!(py, "get"), (intern!(py, "authorization"),))
        .ok()?
        .extract::<String>()
        .ok()
}

/// Credentials that were sent but are malformed; FastAPI rejects them whatever `auto_error` is.
fn invalid<T>(scheme: &Bound<'_, PyAny>) -> PyResult<T> {
    let error = scheme.call_method0(intern!(scheme.py(), "make_not_authenticated_error"))?;
    Err(PyErr::from_value(error))
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

pub(crate) fn source_value(
    request: &Bound<'_, PyAny>,
    source: &ParameterSource,
    name: &str,
) -> Option<String> {
    let py = request.py();
    let attribute = match source {
        ParameterSource::Query => intern!(py, "query_params"),
        ParameterSource::Cookie => intern!(py, "cookies"),
        ParameterSource::Header => intern!(py, "headers"),
        ParameterSource::Path
        | ParameterSource::Body
        | ParameterSource::BackgroundTasks
        | ParameterSource::SecurityScopes => return None,
    };
    request
        .getattr(attribute)
        .ok()?
        .call_method1(intern!(py, "get"), (name,))
        .ok()?
        .extract::<String>()
        .ok()
}

/// Decodes like Python's `b64decode` without `validate`: characters outside the alphabet are
/// skipped, padding is required, and unused trailing bits are allowed.
const LENIENT_BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_allow_trailing_bits(true)
        .with_decode_padding_mode(DecodePaddingMode::RequireCanonical),
);

/// Decodes an RFC 7617 `Basic` credential payload into a `(user, password)` pair, as FastAPI's
/// `HTTPBasic` does: `b64decode(param).decode("ascii")` split at the first colon.
fn decode_basic(payload: &str) -> Option<(String, String)> {
    let filtered: Vec<u8> = payload
        .bytes()
        .filter(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
        .collect();
    let decoded = LENIENT_BASE64.decode(filtered).ok()?;
    if !decoded.is_ascii() {
        return None;
    }
    let text = String::from_utf8(decoded).ok()?;
    let (username, password) = text.split_once(':')?;
    Some((username.to_owned(), password.to_owned()))
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

    if header.is_empty() || !scheme.eq_ignore_ascii_case("basic") {
        return missing(scheme_object, auto_error);
    }

    match decode_basic(payload) {
        Some((username, password)) => Ok(Some(HTTPBasicCredentials { username, password })),
        None => invalid(scheme_object),
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
