pub mod callable;

mod apikey;
mod http;
mod oauth2;

pub use apikey::{APIKeyCookie, APIKeyHeader, APIKeyQuery};
pub use http::{
    HTTPAuthorizationCredentials, HTTPBasic, HTTPBasicCredentials, HTTPBearer, HTTPDigest,
};
pub use oauth2::{OAuth2AuthorizationCodeBearer, OAuth2PasswordBearer, OpenIdConnect};

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use std::sync::Arc;

#[pyclass(
    name = "SecurityScopes",
    module = "fastrapi.security",
    skip_from_py_object
)]
#[derive(Clone, Debug)]
pub struct PySecurityScopes {
    pub scopes: Arc<[String]>,
}

#[pymethods]
impl PySecurityScopes {
    #[new]
    #[pyo3(signature = (scopes=None))]
    pub fn new(scopes: Option<Vec<String>>) -> Self {
        Self {
            scopes: scopes.unwrap_or_default().into(),
        }
    }

    #[getter]
    fn scopes(&self) -> Vec<String> {
        self.scopes.to_vec()
    }

    #[getter]
    fn scope_str(&self) -> String {
        self.scopes.join(" ")
    }
}

#[derive(Clone, Debug)]
pub struct SchemeSpec {
    pub name: String,
    pub description: Option<String>,
    pub scopes: Option<sonic_rs::Value>,
    pub kind: crate::types::route::SecurityKind,
}

fn scopes_json(py: Python<'_>, obj: &Bound<'_, PyAny>) -> Option<sonic_rs::Value> {
    let attr = obj.getattr("scopes").ok()?;
    let dict = attr.cast::<PyDict>().ok()?;
    Some(crate::utils::py_dict_to_json(py, dict))
}

macro_rules! try_scheme {
    ($obj:expr, $ty:ty => $s:ident, $body:expr) => {
        if let Ok(item) = $obj.cast::<$ty>() {
            let $s = &*item.borrow();
            return Some($body);
        }
    };
}

/// Detects one of the security classes and extracts its OpenAPI descriptor
pub fn describe_scheme(py: Python<'_>, obj: &Bound<'_, PyAny>) -> Option<SchemeSpec> {
    use crate::types::route::SecurityKind;
    let _ = py;

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
