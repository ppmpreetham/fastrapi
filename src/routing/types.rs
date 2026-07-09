use crate::router::PyAPIRouter;
use crate::routing::dependencies::DependencyNode;
use crate::types::response::ResponseType;
use ahash::{AHashMap, AHashSet};
use cookie::Cookie;
use pyo3::types::PyString;
use pyo3::{Py, PyAny};
use smallvec::SmallVec;
use std::borrow::Cow;
use std::sync::Arc;
use std::sync::OnceLock;

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
    pub validator_index: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct PathParamRange {
    pub key: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug)]
pub struct UploadedFile {
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub content: Vec<u8>,
}

#[derive(Clone, Debug)]
pub enum BodyField {
    Text(String),
    File(UploadedFile),
}

#[derive(Clone, Debug)]
pub enum BodyPayload {
    Json {
        raw: bytes::Bytes,
        value: Option<sonic_rs::Value>,
    },
    Form(AHashMap<String, BodyField>),
}

#[derive(Clone)]
pub struct PydanticValidator {
    pub name: String,
    pub model_class: Py<PyAny>,
    pub validate_json: Option<Py<PyAny>>,
    pub validate_python: Py<PyAny>,
    pub core_validator: Option<Py<PyAny>>,
    pub validate_json_method: Option<Py<PyAny>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SerializationHint {
    #[default]
    Unknown,
    PydanticModel,
    Dataclass,
    PlainDict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecurityKind {
    OAuth2PasswordBearer { token_url: String, auto_error: bool },
    HTTPBearer { auto_error: bool },
    HTTPBasic { auto_error: bool },
    APIKeyHeader { name: String, auto_error: bool },
    APIKeyQuery { name: String, auto_error: bool },
    APIKeyCookie { name: String, auto_error: bool },
}

#[derive(Clone, Debug)]
pub struct CompiledSecurityScheme {
    pub id: u32,
    pub name: String, // for OpenAPI schema key generation
    pub kind: SecurityKind,
}

#[derive(Clone, Debug)]
pub struct RouteSecurityRequirement {
    pub scheme: Arc<CompiledSecurityScheme>,
    pub scopes: Arc<[String]>, // Zero-allocation cloning for specific route
}

#[derive(Clone, Debug)]
pub enum InjectionType {
    Dependency(usize),
    Parameter(ParsedParameter),
    Request,
    SecurityScopes(usize), // Maps to RouteHandler.security_requirements index
    SecurityScheme(usize), // Maps to RouteHandler.security_requirements index
}

pub struct RequestInput<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query_string: &'a str,

    pub path_params: OnceLock<SmallVec<[(String, &'a str); 8]>>,
    pub query_params: OnceLock<SmallVec<[(Cow<'a, str>, Cow<'a, str>); 8]>>,
    pub headers: &'a axum::http::HeaderMap,
    pub cookies: OnceLock<SmallVec<[(&'a str, &'a str); 8]>>,
}

#[inline(always)]
fn decode_query_component(raw: &str) -> Cow<'_, str> {
    let bytes = raw.as_bytes();
    let has_percent = bytes.contains(&b'%');
    let has_plus = bytes.contains(&b'+');

    if !has_percent && !has_plus {
        return Cow::Borrowed(raw);
    }

    if has_plus {
        let replaced = raw.replace('+', " ");
        Cow::Owned(
            percent_encoding::percent_decode_str(&replaced)
                .decode_utf8_lossy()
                .into_owned(),
        )
    } else {
        Cow::Owned(
            percent_encoding::percent_decode_str(raw)
                .decode_utf8_lossy()
                .into_owned(),
        )
    }
}

impl<'a> RequestInput<'a> {
    pub fn get_path_param(&self, key: &str) -> Option<&'a str> {
        self.path_params
            .get()?
            .iter()
            .find(|(k, _)| k == key)
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
                let key_cow = decode_query_component(k);
                let val_cow = decode_query_component(v);
                result.push((key_cow, val_cow));
            }
            result
        })
    }

    pub fn get_query_param(&self, key: &str) -> Option<Cow<'a, str>> {
        if let Some(query_params) = self.query_params.get() {
            return query_params
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone());
        }

        if self.query_string.is_empty() {
            return None;
        }

        for pair in self.query_string.split('&') {
            if pair.is_empty() {
                continue;
            }

            let (raw_key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
            if raw_key == key || decode_query_component(raw_key).as_ref() == key {
                return Some(decode_query_component(raw_value));
            }
        }

        None
    }

    pub fn get_all_cookies(&self) -> &SmallVec<[(&'a str, &'a str); 8]> {
        self.cookies.get_or_init(|| {
            let mut result = SmallVec::new();
            self.headers
                .get_all(axum::http::header::COOKIE)
                .iter()
                .filter_map(|header_value| header_value.to_str().ok())
                .for_each(|raw_cookie| {
                    Cookie::split_parse(raw_cookie)
                        .filter_map(Result::ok)
                        .filter_map(|cookie| Some((cookie.name_raw()?, cookie.value_raw()?)))
                        .for_each(|cookie| result.push(cookie));
                });

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
    pub param_validators: Vec<PydanticValidator>,
    pub response_type: ResponseType,
    pub serialization_hint: SerializationHint,
    pub needs_kwargs: bool,
    pub body_param_names: Vec<Py<PyString>>,
    pub body_param_name_set: AHashSet<String>,
    pub body_param_indices: SmallVec<[usize; 4]>,
    pub dependencies: Vec<DependencyNode>,
    pub parsed_params: Vec<ParsedParameter>,
    pub default_status: Option<axum::http::StatusCode>,
    pub response_model: Option<Py<PyAny>>,
    pub response_class: Option<Py<PyAny>>,
    pub execution_mode: crate::ffi::py_handlers::ExecutionMode,
    pub cache_response: bool,
    // pub security_requirements: Vec<RouteSecurityRequirement>,
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

    pub summary: Option<String>,
    pub description: Option<String>,
    pub deprecated: Option<bool>,
    pub include_in_schema: bool,
}

#[derive(Clone)]
pub struct FlatWebSocket {
    pub path: String,
    pub handler: Py<PyAny>,
}
