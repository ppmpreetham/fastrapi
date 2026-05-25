use crate::router::PyAPIRouter;
use crate::routing::dependencies::DependencyNode;
use crate::types::response::ResponseType;
use once_cell::sync::OnceCell;
use pyo3::types::PyString;
use pyo3::{Py, PyAny};
use smallvec::SmallVec;
use std::borrow::Cow;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum HttpMethod {
    GET = 0,
    POST = 1,
    PUT = 2,
    DELETE = 3,
    PATCH = 4,
    OPTIONS = 5,
    HEAD = 6,
}

impl HttpMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            HttpMethod::GET => "GET",
            HttpMethod::POST => "POST",
            HttpMethod::PUT => "PUT",
            HttpMethod::DELETE => "DELETE",
            HttpMethod::PATCH => "PATCH",
            HttpMethod::OPTIONS => "OPTIONS",
            HttpMethod::HEAD => "HEAD",
        }
    }
}

pub const HTTP_METHOD_COUNT: usize = 7;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParameterSource {
    Path,
    Query,
    Body,
    Header,
    Cookie,
}

#[derive(Clone, Debug, Default)]
pub struct ParameterConstraints {
    pub gt: Option<f64>,
    pub ge: Option<f64>,
    pub lt: Option<f64>,
    pub le: Option<f64>,
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    pub pattern: Option<Arc<regex::Regex>>,
}

#[derive(Clone, Debug)]
pub struct ParsedParameter {
    pub name: String,
    pub name_py: Py<PyString>,
    pub external_name: String,
    pub source: ParameterSource,
    pub annotation: Option<Py<PyAny>>,
    pub default_value: Option<Py<PyAny>>,
    pub has_default: bool,
    pub required: bool,
    pub description: Option<String>,
    pub constraints: ParameterConstraints,
    pub param_object: Option<Py<PyAny>>,
    pub is_pydantic_model: bool,
    pub scalar_kind: crate::ffi::pydantic::ScalarKind,
}

#[derive(Clone, Debug)]
pub struct PathParamRange {
    pub key: &'static str,
    pub start: usize,
    pub end: usize,
}

pub struct RequestInput<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query_string: &'a str,

    pub path_params: OnceCell<SmallVec<[(&'a str, &'a str); 8]>>,
    pub query_params: OnceCell<SmallVec<[(Cow<'a, str>, Cow<'a, str>); 8]>>,
    pub headers: &'a axum::http::HeaderMap,
    pub cookies: OnceCell<SmallVec<[(&'a str, &'a str); 8]>>,
}

impl<'a> RequestInput<'a> {
    pub fn get_path_param(&self, key: &str) -> Option<&'a str> {
        self.path_params
            .get()?
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
    }

    pub fn get_all_query_params(&self) -> &SmallVec<[(Cow<'a, str>, Cow<'a, str>); 8]> {
        self.query_params.get_or_init(|| {
            let mut result = SmallVec::new();
            if self.query_string.is_empty() {
                return result;
            }
            for pair in self.query_string.split('&') {
                if pair.is_empty() {
                    continue;
                }
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                let key_cow = match k.contains('%') || k.contains('+') {
                    true => Cow::Owned(
                        percent_encoding::percent_decode_str(&k.replace('+', " "))
                            .decode_utf8_lossy()
                            .into_owned(),
                    ),
                    false => Cow::Borrowed(k),
                };
                let val_cow = match v.contains('%') || v.contains('+') {
                    true => Cow::Owned(
                        percent_encoding::percent_decode_str(&v.replace('+', " "))
                            .decode_utf8_lossy()
                            .into_owned(),
                    ),
                    false => Cow::Borrowed(v),
                };
                result.push((key_cow, val_cow));
            }
            result
        })
    }

    pub fn get_query_param(&self, key: &str) -> Option<Cow<'a, str>> {
        self.get_all_query_params()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    pub fn get_all_cookies(&self) -> &SmallVec<[(&'a str, &'a str); 8]> {
        self.cookies.get_or_init(|| {
            let mut result = SmallVec::new();
            for header_value in self.headers.get_all(axum::http::header::COOKIE) {
                let Ok(raw_cookie) = header_value.to_str() else {
                    continue;
                };
                for cookie in raw_cookie.split(';') {
                    let trimmed = cookie.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let (name, value) = trimmed.split_once('=').unwrap_or((trimmed, ""));
                    result.push((name.trim(), value.trim()));
                }
            }
            result
        })
    }

    pub fn get_cookie(&self, key: &str) -> Option<&'a str> {
        self.get_all_cookies()
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
    }

    pub fn get_header(&self, key: &str) -> Option<&'a str> {
        self.headers.get(key).and_then(|v| v.to_str().ok())
    }
}

#[derive(Clone)]
pub struct RouteHandler {
    pub func: Py<PyAny>,
    pub is_async: bool,
    pub is_fast_path: bool,
    pub dependency_needs_request: bool,
    pub all_deps_sync: bool,
    pub param_validators: Vec<(String, Py<PyAny>)>,
    pub response_type: ResponseType,
    pub needs_kwargs: bool,
    pub kwargs_template: Option<Py<pyo3::types::PyDict>>,
    pub body_param_names: Vec<Py<PyString>>,
    pub dependencies: Vec<DependencyNode>,
    pub parsed_params: Vec<ParsedParameter>,
    pub default_status: Option<axum::http::StatusCode>,
    pub response_model: Option<Py<PyAny>>,
    pub response_class: Option<Py<PyAny>>,
    pub execution_mode: crate::ffi::py_handlers::ExecutionMode,
}

#[derive(Clone)]
pub struct RouteEntry {
    pub method: HttpMethod,
    pub path: String,
    pub handler: Arc<RouteHandler>,
    pub tags: Vec<String>,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub deprecated: Option<bool>,
    pub include_in_schema: bool,
}

#[derive(Clone)]
pub struct WebSocketEntry {
    pub path: String,
    pub handler: Py<PyAny>,
}

#[derive(Clone)]
pub struct SubRouterMount {
    pub router: Py<PyAPIRouter>,
    pub prefix: String,
    pub tags: Vec<String>,
}

#[derive(Clone)]
pub struct FlatRoute {
    pub method: HttpMethod,
    pub path: String,
    pub handler: Arc<RouteHandler>,
    pub tags: Vec<String>,
}

#[derive(Clone)]
pub struct FlatWebSocket {
    pub path: String,
    pub handler: Py<PyAny>,
}
