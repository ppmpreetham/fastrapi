//! Converts a Python security object into a `SchemeSpec` value.
//! The OpenAPI schema and the route security requirements use this value.

use fastrapi_core::security::SecurityKind;
use fastrapi_pyutil::json::py_dict_to_json;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};

use super::{
    APIKeyCookie, APIKeyHeader, APIKeyQuery, HTTPBasic, HTTPBearer, HTTPDigest,
    OAuth2AuthorizationCodeBearer, OAuth2PasswordBearer, OpenIdConnect,
};

#[derive(Clone, Debug)]
pub struct SchemeSpec {
    pub name: String,
    pub description: Option<String>,
    pub scopes: Option<simd_json::OwnedValue>,
    pub kind: SecurityKind,
}

fn scopes_json(py: Python<'_>, obj: &Bound<'_, PyAny>) -> Option<simd_json::OwnedValue> {
    let attr = obj.getattr("scopes").ok()?;
    let dict = attr.cast::<PyDict>().ok()?;
    Some(py_dict_to_json(py, dict))
}

macro_rules! try_scheme {
    ($obj:expr, $ty:ty => $s:ident, $body:expr) => {
        if let Ok(item) = $obj.cast::<$ty>() {
            let $s = &*item.borrow();
            return Some($body);
        }
    };
}

#[must_use]
pub fn describe_scheme(py: Python<'_>, obj: &Bound<'_, PyAny>) -> Option<SchemeSpec> {
    use fastrapi_core::security::SecurityKind;
    _ = py;

    try_scheme!(obj, OAuth2PasswordBearer => s, SchemeSpec {
        name: s.scheme_name.clone().unwrap_or_else(|| "OAuth2".into()),
        description: s.description.clone(),
        scopes: scopes_json(py, obj),
        kind: SecurityKind::OAuth2PasswordBearer {
            token_url: s.token_url.clone(),
            auto_error: s.auto_error,
        },
    });
    try_scheme!(obj, OAuth2AuthorizationCodeBearer => s, SchemeSpec {
        name: s.scheme_name.clone().unwrap_or_else(|| "OAuth2".into()),
        description: s.description.clone(),
        scopes: scopes_json(py, obj),
        kind: SecurityKind::OAuth2AuthorizationCode {
            authorization_url: s.authorization_url.clone(),
            token_url: s.token_url.clone(),
            refresh_url: s.refresh_url.clone(),
            auto_error: s.auto_error,
        },
    });
    try_scheme!(obj, OpenIdConnect => s, SchemeSpec {
        name: s.scheme_name.clone().unwrap_or_else(|| "OpenIdConnect".into()),
        description: s.description.clone(),
        scopes: None,
        kind: SecurityKind::OpenIdConnect {
            url: s.openid_connect_url.clone(),
            auto_error: s.auto_error,
        },
    });
    try_scheme!(obj, HTTPDigest => s, SchemeSpec {
        name: s.scheme_name.clone().unwrap_or_else(|| "HTTPDigest".into()),
        description: s.description.clone(),
        scopes: None,
        kind: SecurityKind::HTTPDigest { auto_error: s.auto_error },
    });
    try_scheme!(obj, HTTPBearer => s, SchemeSpec {
        name: s.scheme_name.clone().unwrap_or_else(|| "HTTPBearer".into()),
        description: s.description.clone(),
        scopes: None,
        kind: SecurityKind::HTTPBearer {
            auto_error: s.auto_error,
            bearer_format: s.bearer_format.clone(),
        },
    });
    try_scheme!(obj, HTTPBasic => s, SchemeSpec {
        name: s.scheme_name.clone().unwrap_or_else(|| "HTTPBasic".into()),
        description: s.description.clone(),
        scopes: None,
        kind: SecurityKind::HTTPBasic { auto_error: s.auto_error },
    });
    try_scheme!(obj, APIKeyHeader => s, SchemeSpec {
        name: s.scheme_name.clone().unwrap_or_else(|| format!("APIKeyHeader-{}", s.name)),
        description: s.description.clone(),
        scopes: None,
        kind: SecurityKind::APIKeyHeader { name: s.name.clone(), auto_error: s.auto_error },
    });
    try_scheme!(obj, APIKeyQuery => s, SchemeSpec {
        name: s.scheme_name.clone().unwrap_or_else(|| format!("APIKeyQuery-{}", s.name)),
        description: s.description.clone(),
        scopes: None,
        kind: SecurityKind::APIKeyQuery { name: s.name.clone(), auto_error: s.auto_error },
    });
    try_scheme!(obj, APIKeyCookie => s, SchemeSpec {
        name: s.scheme_name.clone().unwrap_or_else(|| format!("APIKeyCookie-{}", s.name)),
        description: s.description.clone(),
        scopes: None,
        kind: SecurityKind::APIKeyCookie { name: s.name.clone(), auto_error: s.auto_error },
    });

    None
}
