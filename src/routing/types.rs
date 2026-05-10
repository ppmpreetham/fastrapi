use crate::router::PyAPIRouter;
use crate::routing::dependencies::DependencyNode;
use crate::types::response::ResponseType;
use pyo3::types::PyString;
use pyo3::{Py, PyAny};
use std::collections::HashMap;
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

#[derive(Clone, Debug, Default)]
pub struct RequestInput {
    pub method: String,
    pub path: String,
    pub query_string: String,
    pub path_params: HashMap<String, String>,
    pub query_params: HashMap<String, String>,
    pub headers: HashMap<String, String>,
    pub cookies: HashMap<String, String>,
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
    pub path_param_names: Vec<Py<PyString>>,
    pub query_param_names: Vec<Py<PyString>>,
    pub body_param_names: Vec<Py<PyString>>,
    pub dependencies: Vec<DependencyNode>,
    pub parsed_params: Vec<ParsedParameter>,
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
