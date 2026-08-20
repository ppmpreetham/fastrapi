use base64::Engine as _;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyString};

use crate::ffi::exceptions::PyHTTPException;

/// fastrapi.HTTPException(status, detail)
fn http_exc(py: Python<'_>, status: u16, detail: &str) -> PyErr {
    let cls = py.get_type::<PyHTTPException>();
    let detail = PyString::new(py, detail);
    match cls.call1((status, detail)) {
        Ok(value) => PyErr::from_value(value),
        Err(err) => err,
    }
}

/// 401 with the standard challenge semantics.
pub(crate) fn unauthorized(py: Python<'_>) -> PyErr {
    http_exc(py, 401, "Not authenticated")
}

/// 403 for authenticated-but-invalid credentials.
pub(crate) fn forbidden(py: Python<'_>) -> PyErr {
    http_exc(py, 403, "Invalid authentication credentials")
}

pub(crate) fn not_authenticated_403(py: Python<'_>) -> PyErr {
    http_exc(py, 403, "Not authenticated")
}

fn split_authorization(header: &str) -> Option<(String, String)> {
    let (scheme, credentials) = header.split_once(' ')?;
    Some((scheme.to_ascii_lowercase(), credentials.trim().to_owned()))
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

pub(crate) fn bearer_token(request: &Bound<'_, PyAny>, auto_error: bool) -> Result<String, PyErr> {
    let py = request.py();
    let token = auth_header(request)
        .as_deref()
        .and_then(split_authorization)
        .filter(|(scheme, _)| scheme == "bearer")
        .map(|(_, credentials)| credentials);

    match token {
        Some(token) if !token.is_empty() => Ok(token),
        Some(_) if auto_error => Err(forbidden(py)),
        Some(token) => Ok(token),
        None if auto_error => Err(unauthorized(py)),
        None => Ok(String::new()),
    }
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

/// Decodes an RFC 7617 `Basic` credential payload into `(user, password)`.
fn decode_basic(payload: &str) -> Option<(String, String)> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let colon = text.find(':')?;
    Some((text[..colon].to_owned(), text[colon + 1..].to_owned()))
}

pub(crate) fn http_bearer_call(
    auto_error: bool,
    bearer_format: Option<&str>,
    request: &Bound<'_, PyAny>,
) -> PyResult<crate::routing::security::HTTPAuthorizationCredentials> {
    let _ = bearer_format;
    let header = auth_header(request);
    let parts = header.as_deref().and_then(split_authorization);

    bearer_token(request, auto_error)?;

    match parts.filter(|(_, credentials)| !credentials.is_empty()) {
        Some((scheme, credentials)) => Ok(crate::routing::security::HTTPAuthorizationCredentials {
            scheme,
            credentials,
        }),
        None if auto_error => Err(forbidden(request.py())),
        None => Ok(crate::routing::security::HTTPAuthorizationCredentials {
            scheme: String::new(),
            credentials: String::new(),
        }),
    }
}

pub(crate) fn http_basic_call(
    auto_error: bool,
    request: &Bound<'_, PyAny>,
) -> PyResult<crate::routing::security::HTTPBasicCredentials> {
    let credentials = auth_header(request)
        .as_deref()
        .and_then(split_authorization)
        .filter(|(scheme, _)| scheme == "basic")
        .and_then(|(_, payload)| decode_basic(&payload));

    match credentials {
        Some((username, password)) => {
            Ok(crate::routing::security::HTTPBasicCredentials { username, password })
        }
        None if auto_error => Err(unauthorized(request.py())),
        None => Ok(crate::routing::security::HTTPBasicCredentials {
            username: String::new(),
            password: String::new(),
        }),
    }
}

pub(crate) fn api_key_call(
    source: &'static str,
    name: &str,
    auto_error: bool,
    request: &Bound<'_, PyAny>,
) -> PyResult<String> {
    match source_value(request, source, name).filter(|key| !key.is_empty()) {
        Some(key) => Ok(key),
        None if auto_error => Err(not_authenticated_403(request.py())),
        None => Ok(String::new()),
    }
}
