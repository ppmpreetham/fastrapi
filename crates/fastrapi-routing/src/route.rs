//! Data types that describe a route after the analysis.
//! These types hold the parsed parameters, the validators, the execution plan, and the route entries for HTTP and WebSocket routes.

use crate::dependencies::{DependencyNode, InjectionType, OverrideCache};
use crate::kernel_params::ParamKernel;
use crate::kernel_validation::KernelFastPath;
use fastrapi_core::method::HttpMethod;
use fastrapi_core::params::{ParameterConstraints, ParameterSource};
use fastrapi_core::security::RouteSecurityRequirement;
use fastrapi_http::response_type::ResponseFormatter;
use fastrapi_pyutil::call::CallLayout;
use pyo3::types::PyString;
use pyo3::{Py, PyAny};
use smallvec::SmallVec;
use std::num::NonZeroU32;
use std::sync::{Arc, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FastKind {
    Str,
    Int,
    Float,
    Bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmptyDefault {
    List,
    Dict,
    Set,
}

#[derive(Clone, Debug)]
pub struct ModelKey {
    pub lookup: String,
    pub alias: Py<PyString>,
    pub sequence: bool,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Parsed parameter tracks multiple boolean validation and extraction flags"
)]
#[derive(Clone, Debug)]
pub struct ParsedParameter {
    pub name: String,
    pub name_py: Py<PyString>,
    pub external_name: String,
    pub alias_py: Py<PyString>,
    pub header_name: Option<axum::http::HeaderName>,
    pub source: ParameterSource,
    pub annotation: Option<Py<PyAny>>,
    pub default_value: Option<Py<PyAny>>,
    pub make_default: Option<Py<PyAny>>,
    pub has_default: bool,
    pub required: bool,
    pub is_list: bool,
    pub description: Option<String>,
    pub constraints: ParameterConstraints,
    pub param_object: Option<Py<PyAny>>,
    pub is_pydantic_model: bool,
    pub model_keys: Option<Arc<[ModelKey]>>,
    pub loc_renames: Option<Py<PyAny>>,
    pub is_file: bool,
    pub is_form: bool,
    pub read_bytes: bool,
    pub embed: bool,
    pub fast: Option<FastKind>,
    pub fast_list: Option<FastKind>,
    pub fast_pattern: Option<regex::Regex>,
    pub empty_default: Option<EmptyDefault>,
    pub kernel: Option<Arc<ParamKernel>>,
    pub validate: Py<PyAny>,
    pub validate_json: Option<Py<PyAny>>,
    pub validator_index: Option<usize>,
}

#[derive(Clone)]
pub struct PydanticValidator {
    pub name: String,
    pub model_class: Py<PyAny>,
    pub kernel: OnceLock<Option<Arc<KernelFastPath>>>,
    pub validate_json: Option<Py<PyAny>>,
    pub validate_python: Py<PyAny>,
    pub core_validator: Option<Py<PyAny>>,
    pub validate_json_method: Option<Py<PyAny>>,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Execution plan tracks independent dispatch characteristics"
)]
#[derive(Clone)]
pub struct ExecutionPlan {
    pub func: Py<PyAny>,
    pub call_layout: Option<Arc<CallLayout>>,
    pub is_async: bool,
    pub inline_async: bool,
    pub is_fast_path: bool,
    pub execution_mode: ExecutionMode,
    pub cache_response: bool,
    pub rate_limit_per_second: Option<NonZeroU32>,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Payload spec tracks multiple boolean dispatch and dependency characteristics"
)]
#[derive(Clone)]
pub struct PayloadSpec {
    pub dependency_needs_request: bool,
    pub all_deps_sync: bool,
    pub needs_kwargs: bool,
    pub dependency_needs_response: bool,
    pub has_generators: bool,
    pub has_async_dependencies: bool,
    pub request_param: Option<Py<PyString>>,
    pub response_param: Option<Py<PyString>>,
    pub dependency_overrides: Option<Py<PyAny>>,
    pub body_param_names: Vec<Py<PyString>>,
    pub body_param_indices: SmallVec<[usize; 4]>,
    pub body_embed: bool,
    pub body_is_form: bool,
    pub strict_content_type: Option<bool>,
    pub request_param_indices: SmallVec<[usize; 8]>,
    pub needs_body: bool,
    pub override_cache: OverrideCache,
    pub dependencies: Vec<DependencyNode>,
    pub parsed_params: Vec<ParsedParameter>,
    pub has_multiple_query_params: bool,
    pub path_param_names: Vec<Arc<str>>,
}

impl PayloadSpec {
    pub fn refresh_dependency_flags(&mut self) {
        let deps = &self.dependencies;
        self.dependency_needs_request = deps.iter().any(|node| node.needs_request_object);
        self.dependency_needs_response = deps.iter().any(|node| {
            node.injection_plan
                .iter()
                .any(|(_, injection)| matches!(injection, InjectionType::Response))
        });
        self.has_generators = deps
            .iter()
            .any(|node| node.is_generator || node.is_async_generator);
        self.has_async_dependencies = deps
            .iter()
            .any(|node| node.is_async || node.is_async_generator);
        self.all_deps_sync = deps.iter().all(DependencyNode::is_sync_callable);
    }
}

#[derive(Clone)]
pub struct ValidationRules {
    pub param_validators: Vec<PydanticValidator>,
    pub bypass_serialization: bool,
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
    pub name: String,
    pub generate_unique_id_function: Option<Py<PyAny>>,
    pub explicit_response_class: bool,
    pub route_object: Arc<parking_lot::Mutex<Option<Py<PyAny>>>>,
    pub handler: Arc<RouteHandler>,
    pub route_instance: Option<Py<PyAny>>,
    pub tags: Vec<String>,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub response_description: Option<String>,
    pub operation_id: Option<String>,
    pub openapi_extra: Option<simd_json::OwnedValue>,
    pub responses: Option<Py<PyAny>>,
    pub callbacks: Option<simd_json::OwnedValue>,
    pub callback_routes: Option<Py<PyAny>>,
    pub deprecated: Option<bool>,
    pub include_in_schema: bool,
    pub security: Vec<RouteSecurityRequirement>,
    pub host: Option<String>,
}

#[derive(Clone)]
pub struct WebSocketEntry {
    pub path: String,
    pub name: String,
    pub handler: Py<PyAny>,
    pub websocket_arg: Option<Py<PyString>>,
    pub params: Vec<ParsedParameter>,
    pub deps: SmallVec<[DependencyNode; 4]>,
    pub dependency_overrides: Option<Py<PyAny>>,
    pub host: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    SyncNoArgs,
    AsyncNoArgs,
    SyncNoDeps,
    SyncDepsNoReq,
    SyncDepsReq,
    AsyncNoDeps,
    AsyncSyncDepsNoReq,
    AsyncSyncDepsReq,
    AsyncAsyncDepsNoReq,
    AsyncAsyncDepsReq,
    Managed,
}

impl RouteHandler {
    pub fn assign_execution_mode(&mut self) {
        self.payload.needs_body = !self.payload.body_param_indices.is_empty()
            || self.payload.request_param.is_some()
            || self.payload.dependency_needs_request;
        let payload = &self.payload;
        if payload.response_param.is_some()
            || payload.dependency_needs_response
            || payload.has_generators
            || (payload.has_async_dependencies && !self.execution.is_async)
        {
            self.execution.execution_mode = ExecutionMode::Managed;
            return;
        }

        let is_async = self.execution.is_async;
        let deps_empty = self.payload.dependencies.is_empty();
        let all_sync = self.payload.all_deps_sync;
        let needs_req = self.payload.dependency_needs_request;
        let needs_kwargs =
            self.payload.needs_kwargs || self.payload.response_param.is_some() || !deps_empty;

        self.execution.execution_mode =
            match (needs_kwargs, is_async, deps_empty, all_sync, needs_req) {
                (false, false, _, _, _) => ExecutionMode::SyncNoArgs,
                (false, true, _, _, _) => ExecutionMode::AsyncNoArgs,

                (true, false, true, _, _) => ExecutionMode::SyncNoDeps,
                (true, false, false, _, false) => ExecutionMode::SyncDepsNoReq,
                (true, false, false, _, true) => ExecutionMode::SyncDepsReq,

                (true, true, true, _, _) => ExecutionMode::AsyncNoDeps,
                (true, true, false, true, false) => ExecutionMode::AsyncSyncDepsNoReq,
                (true, true, false, true, true) => ExecutionMode::AsyncSyncDepsReq,
                (true, true, false, false, false) => ExecutionMode::AsyncAsyncDepsNoReq,
                (true, true, false, false, true) => ExecutionMode::AsyncAsyncDepsReq,
            };
    }
}
