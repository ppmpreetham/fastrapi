//! Converts a Python security object into a `SchemeSpec` value.
//! The OpenAPI schema and the route security requirements use this value.

use fastrapi_core::security::SecurityKind;
use fastrapi_pyutil::json::{py_any_to_json, py_dict_to_json};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyString};

use super::{
    APIKeyCookie, APIKeyHeader, APIKeyQuery, HTTPBasic, HTTPBearer, HTTPDigest, OAuth2,
    OAuth2AuthorizationCodeBearer, OAuth2PasswordBearer, OpenIdConnect,
};

fastrapi_pyutil::cached_py_import!(SECURITY_BASE, "fastrapi.security.base", "SecurityBase");
fastrapi_pyutil::cached_py_import!(API_KEY_MODEL, "fastrapi.openapi.models", "APIKey");
fastrapi_pyutil::cached_py_import!(HTTP_MODEL, "fastrapi.openapi.models", "HTTPBase");
fastrapi_pyutil::cached_py_import!(BEARER_MODEL, "fastrapi.openapi.models", "HTTPBearer");
fastrapi_pyutil::cached_py_import!(OAUTH2_MODEL, "fastrapi.openapi.models", "OAuth2");
fastrapi_pyutil::cached_py_import!(OPENID_MODEL, "fastrapi.openapi.models", "OpenIdConnect");

fn build_model<'py>(
    model: &Bound<'py, PyAny>,
    fields: &[(&str, &Bound<'py, PyAny>)],
    description: Option<&str>,
) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(model.py());
    for (name, value) in fields {
        kwargs.set_item(*name, value)?;
    }
    kwargs.set_item("description", description)?;
    model.call((), Some(&kwargs))
}

pub(crate) fn api_key_model<'py>(
    py: Python<'py>,
    location: &str,
    name: &str,
    description: Option<&str>,
) -> PyResult<Bound<'py, PyAny>> {
    let location = PyString::new(py, location).into_any();
    let name = PyString::new(py, name).into_any();
    build_model(
        API_KEY_MODEL.get(py)?,
        &[("in", &location), ("name", &name)],
        description,
    )
}

pub(crate) fn http_model<'py>(
    py: Python<'py>,
    scheme: &str,
    description: Option<&str>,
) -> PyResult<Bound<'py, PyAny>> {
    let scheme = PyString::new(py, scheme).into_any();
    build_model(HTTP_MODEL.get(py)?, &[("scheme", &scheme)], description)
}

pub(crate) fn bearer_model<'py>(
    py: Python<'py>,
    bearer_format: Option<&str>,
    description: Option<&str>,
) -> PyResult<Bound<'py, PyAny>> {
    let bearer_format = bearer_format.into_pyobject(py)?.into_any();
    build_model(
        BEARER_MODEL.get(py)?,
        &[("bearerFormat", &bearer_format)],
        description,
    )
}

pub(crate) fn oauth2_model<'py>(
    flows: &Bound<'py, PyAny>,
    description: Option<&str>,
) -> PyResult<Bound<'py, PyAny>> {
    build_model(
        OAUTH2_MODEL.get(flows.py())?,
        &[("flows", flows)],
        description,
    )
}

pub(crate) fn oauth2_flow<'py>(
    py: Python<'py>,
    flow: &str,
    urls: &[(&str, Option<&str>)],
    scopes: Option<&Py<PyDict>>,
) -> PyResult<Bound<'py, PyAny>> {
    let settings = PyDict::new(py);
    for (name, url) in urls {
        settings.set_item(*name, url)?;
    }
    match scopes {
        Some(scopes) => settings.set_item("scopes", scopes)?,
        None => settings.set_item("scopes", PyDict::new(py))?,
    }
    let flows = PyDict::new(py);
    flows.set_item(flow, settings)?;
    Ok(flows.into_any())
}

pub(crate) fn openid_model<'py>(
    py: Python<'py>,
    url: &str,
    description: Option<&str>,
) -> PyResult<Bound<'py, PyAny>> {
    let url = PyString::new(py, url).into_any();
    build_model(
        OPENID_MODEL.get(py)?,
        &[("openIdConnectUrl", &url)],
        description,
    )
}

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
        name: s.scheme_name.clone().unwrap_or_else(|| "OAuth2PasswordBearer".into()),
        description: s.description.clone(),
        scopes: scopes_json(py, obj),
        kind: SecurityKind::OAuth2PasswordBearer {
            token_url: s.token_url.clone(),
            refresh_url: s.refresh_url.clone(),
            auto_error: s.auto_error,
        },
    });
    try_scheme!(obj, OAuth2AuthorizationCodeBearer => s, SchemeSpec {
        name: s.scheme_name.clone().unwrap_or_else(|| "OAuth2AuthorizationCodeBearer".into()),
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
        name: s.scheme_name.clone().unwrap_or_else(|| "APIKeyHeader".into()),
        description: s.description.clone(),
        scopes: None,
        kind: SecurityKind::APIKeyHeader { name: s.name.clone(), auto_error: s.auto_error },
    });
    try_scheme!(obj, APIKeyQuery => s, SchemeSpec {
        name: s.scheme_name.clone().unwrap_or_else(|| "APIKeyQuery".into()),
        description: s.description.clone(),
        scopes: None,
        kind: SecurityKind::APIKeyQuery { name: s.name.clone(), auto_error: s.auto_error },
    });
    try_scheme!(obj, OAuth2 => s, SchemeSpec {
        name: s.scheme_name.clone().unwrap_or_else(|| "OAuth2".into()),
        description: s.description.clone(),
        scopes: None,
        kind: SecurityKind::OAuth2 {
            flows: py_any_to_json(py, s.flows.bind(py)),
            auto_error: s.auto_error,
        },
    });
    try_scheme!(obj, APIKeyCookie => s, SchemeSpec {
        name: s.scheme_name.clone().unwrap_or_else(|| "APIKeyCookie".into()),
        description: s.description.clone(),
        scopes: None,
        kind: SecurityKind::APIKeyCookie { name: s.name.clone(), auto_error: s.auto_error },
    });

    model_scheme(obj).ok().flatten()
}

fn model_scheme(obj: &Bound<'_, PyAny>) -> PyResult<Option<SchemeSpec>> {
    let py = obj.py();
    if !obj.is_instance(SECURITY_BASE.get(py)?)? {
        return Ok(None);
    }
    let kwargs = PyDict::new(py);
    kwargs.set_item("mode", "json")?;
    kwargs.set_item("by_alias", true)?;
    kwargs.set_item("exclude_none", true)?;
    let scheme = obj
        .getattr("model")?
        .call_method("model_dump", (), Some(&kwargs))?;
    Ok(Some(SchemeSpec {
        name: obj.getattr("scheme_name")?.extract()?,
        description: None,
        scopes: None,
        kind: SecurityKind::Model {
            scheme: py_any_to_json(py, &scheme),
        },
    }))
}
