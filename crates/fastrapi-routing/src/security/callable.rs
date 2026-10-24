//! The Rust logic for the `__call__` methods of the security classes.
//! The functions read a bearer token, HTTP Basic credentials, or an API key from a request.
//! When the credentials are missing or not valid, they raise an `HTTPException`.

use base64::Engine as _;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyString};

use crate::security::{HTTPAuthorizationCredentials, HTTPBasicCredentials};
use base64::engine::general_purpose::STANDARD;
use fastrapi_http::exceptions::PyHTTPException;

/// Makes an `HTTPException` error with a status code, a detail, and an optional challenge.
/// RFC 9110 requires a `WWW-Authenticate` header in a 401 response.
fn http_exc(py: Python<'_>, status: u16, detail: &str, challenge: Option<&str>) -> PyErr {
    let detail = PyString::new(py, detail);
    let raised = match challenge {
        Some(value) => {
            let headers = PyDict::new(py);
            match headers.set_item(intern!(py, "WWW-Authenticate"), value) {
                Ok(()) => py
                    .get_type::<PyHTTPException>()
                    .call1((status, detail, headers)),
                Err(err) => return err,
            }
        }
        None => py.get_type::<PyHTTPException>().call1((status, detail)),
    };
    match raised {
        Ok(value) => PyErr::from_value(value),
        Err(err) => err,
    }
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

pub(crate) fn bearer_token(request: &Bound<'_, PyAny>, auto_error: bool) -> Result<String, PyErr> {
    let header = auth_header(request).unwrap_or_default();
    let (scheme, credentials) = split_authorization(&header);

    if header.is_empty() || !scheme.eq_ignore_ascii_case("bearer") {
        return if auto_error {
            Err(unauthorized(request.py(), "Bearer"))
        } else {
            Ok(String::new())
        };
    }
    Ok(credentials.to_owned())
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
) -> PyResult<HTTPAuthorizationCredentials> {
    _ = bearer_format;
    let header = auth_header(request).unwrap_or_default();
    let (scheme, credentials) = split_authorization(&header);

    if header.is_empty() || !scheme.eq_ignore_ascii_case("bearer") || credentials.is_empty() {
        return if auto_error {
            Err(unauthorized(request.py(), "Bearer"))
        } else {
            Ok(empty_credentials())
        };
    }

    Ok(HTTPAuthorizationCredentials {
        scheme: scheme.to_owned(),
        credentials: credentials.to_owned(),
    })
}

const fn empty_credentials() -> HTTPAuthorizationCredentials {
    HTTPAuthorizationCredentials {
        scheme: String::new(),
        credentials: String::new(),
    }
}

pub(crate) fn http_basic_call(
    auto_error: bool,
    realm: Option<&str>,
    request: &Bound<'_, PyAny>,
) -> PyResult<HTTPBasicCredentials> {
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
        Some((username, password)) => Ok(HTTPBasicCredentials { username, password }),
        None if auto_error => Err(unauthorized(request.py(), &challenge)),
        None => Ok(HTTPBasicCredentials {
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
        None if auto_error => Err(unauthorized(request.py(), "APIKey")),
        None => Ok(String::new()),
    }
}
