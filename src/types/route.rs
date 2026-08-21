use crate::decorators::PyAPIRouter;
use crate::routing::dependencies::DependencyNode;
use crate::types::response::ResponseType;
use ahash::{AHashMap, AHashSet};
use axum::http::Method;
use pyo3::types::{PyDict, PyString};
use pyo3::{Py, PyAny};
use regex::Regex;
use smallvec::SmallVec;
use std::sync::Arc;
use strum::{AsRefStr, Display, EnumCount, EnumIter, EnumString};

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, AsRefStr, Display, EnumString, EnumCount, EnumIter,
)]
#[repr(u8)]
pub enum HttpMethod {
    GET = 0,
    POST = 1,
    PUT = 2,
    DELETE = 3,
    PATCH = 4,
    OPTIONS = 5,
    HEAD = 6,
}

impl TryFrom<&Method> for HttpMethod {
    type Error = ();

    fn try_from(method: &Method) -> Result<Self, Self::Error> {
        Ok(match *method {
            Method::GET => HttpMethod::GET,
            Method::POST => HttpMethod::POST,
            Method::PUT => HttpMethod::PUT,
            Method::DELETE => HttpMethod::DELETE,
            Method::PATCH => HttpMethod::PATCH,
            Method::OPTIONS => HttpMethod::OPTIONS,
            Method::HEAD => HttpMethod::HEAD,
            _ => return Err(()),
        })
    }
}

pub const HTTP_METHOD_COUNT: usize = HttpMethod::COUNT;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParameterSource {
    Path,
    Query,
    Body,
    Header,
    Cookie,
    BackgroundTasks,
}

#[derive(Clone, Debug, Default)]
pub struct ParameterConstraints {
    pub gt: Option<f64>,
    pub ge: Option<f64>,
    pub lt: Option<f64>,
    pub le: Option<f64>,
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    pub pattern: Option<Arc<Regex>>,
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
    pub is_list: bool,
    pub description: Option<String>,
    pub constraints: ParameterConstraints,
    pub param_object: Option<Py<PyAny>>,
    pub is_pydantic_model: bool,
    pub is_file: bool,
    pub scalar_kind: crate::ffi::pydantic::ScalarKind,
    pub validator_index: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct PathParamRange {
    pub key: Arc<str>,
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
    Form(AHashMap<String, SmallVec<[BodyField; 2]>>),
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
    OAuth2PasswordBearer {
        token_url: String,
        auto_error: bool,
    },
    OAuth2AuthorizationCode {
        authorization_url: String,
        token_url: String,
        refresh_url: Option<String>,
        auto_error: bool,
    },
    OpenIdConnect {
        url: String,
        auto_error: bool,
    },
    HTTPBearer {
        auto_error: bool,
        bearer_format: Option<String>,
    },
    HTTPBasic {
        auto_error: bool,
    },
    HTTPDigest {
        auto_error: bool,
    },
    APIKeyHeader {
        name: String,
        auto_error: bool,
    },
    APIKeyQuery {
        name: String,
        auto_error: bool,
    },
    APIKeyCookie {
        name: String,
        auto_error: bool,
    },
}

#[derive(Clone, Debug)]
pub struct CompiledSecurityScheme {
    pub id: u32,
    pub name: String,
    pub description: Option<String>,
    pub scopes: Option<sonic_rs::Value>,
    pub kind: SecurityKind,
}

#[derive(Clone, Debug)]
pub struct RouteSecurityRequirement {
    pub scheme: Arc<CompiledSecurityScheme>,
    pub scopes: Arc<[String]>,
}

#[derive(Clone)]
pub struct ExecutionPlan {
    pub func: Py<PyAny>,
    pub is_async: bool,
    pub is_fast_path: bool,
    pub execution_mode: crate::runtime::executor::ExecutionMode,
    pub cache_response: bool,
    pub rate_limit_per_second: Option<u32>,
}

#[derive(Clone)]
pub struct PayloadSpec {
    pub dependency_needs_request: bool,
    pub all_deps_sync: bool,
    pub needs_kwargs: bool,
    pub request_param: Option<Py<PyString>>,
    pub body_param_names: Vec<Py<PyString>>,
    pub body_param_name_set: AHashSet<String>,
    pub body_param_indices: SmallVec<[usize; 4]>,
    pub dependencies: Vec<DependencyNode>,
    pub parsed_params: Vec<ParsedParameter>,
    pub has_multiple_query_params: bool,
    pub path_param_names: Vec<Arc<str>>,
}

#[derive(Clone)]
pub struct ValidationRules {
    pub param_validators: Vec<PydanticValidator>,
    pub defer_json_parse: bool,
    pub bypass_serialization: bool,
}

#[derive(Clone)]
pub struct ResponseFormatter {
    pub response_type: ResponseType,
    pub serialization_hint: SerializationHint,
    pub default_status: Option<axum::http::StatusCode>,
    pub response_model: Option<Py<PyAny>>,
    pub response_class: Option<Py<PyAny>>,
    pub dump_options: Option<Py<PyDict>>,
}

#[derive(Clone)]
pub struct RouteHandler {
    pub execution: ExecutionPlan,
    pub payload: PayloadSpec,
    pub validation: ValidationRules,
    pub response: ResponseFormatter,
}

#[derive(Clone)]
pub struct RouteEntry {
    pub method: HttpMethod,
    pub path: String,
    pub handler: Arc<RouteHandler>,
    pub tags: Vec<String>,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub response_description: Option<String>,
    pub operation_id: Option<String>,
    pub openapi_extra: Option<sonic_rs::Value>,
    pub responses: Option<sonic_rs::Value>,
    pub callbacks: Option<sonic_rs::Value>,
    pub deprecated: Option<bool>,
    pub include_in_schema: bool,
    pub security: Vec<RouteSecurityRequirement>,
}

#[derive(Clone)]
pub struct WebSocketEntry {
    pub path: String,
    pub handler: Py<PyAny>,
    pub deps: SmallVec<[DependencyNode; 4]>,
}

#[derive(Clone)]
pub struct SubRouterMount {
    pub router: Py<PyAPIRouter>,
    pub prefix: String,
    pub tags: Vec<String>,
    pub dependencies: Option<Py<PyAny>>,
    pub responses: Option<Py<PyAny>>,
    pub deprecated: Option<bool>,
    pub include_in_schema: bool,
    pub default_response_class: Option<Py<PyAny>>,
    pub generate_unique_id_function: Option<Py<PyAny>>,
}
