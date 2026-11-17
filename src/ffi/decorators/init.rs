//! The construction of route decorators for `APIRouter`.
//! It reads the route options from the keyword arguments and analyzes the route handler.
//! It then adds the HTTP route or the WebSocket route to the router.

use super::router::PyAPIRouter;
use ahash::AHashSet;
use fastrapi_core::method::HttpMethod;
use fastrapi_core::params::ParameterSource;
use fastrapi_core::path::{extract_path_convertors, extract_path_param_names};
use fastrapi_http::response_convert::prepare_response_model;
use fastrapi_http::response_convert::response_type_adapter;
use fastrapi_http::response_type::ResponseFormatter;
use fastrapi_openapi::paths::{endpoint_description, parse_callbacks_to_json};
use fastrapi_pyutil::call::CallLayout;
use fastrapi_pyutil::json::{SerializationHint, json_to_py_object, py_any_to_json};
use fastrapi_routing::analysis::{
    ParsedRouteMetadata, parse_route_metadata, parse_websocket_signature,
};
use fastrapi_routing::dependencies::{
    DependencyNode, OverrideCache, collect_dependency_markers, merge_dependencies,
    parse_dependencies, parse_external_dependencies,
};
use fastrapi_routing::route::{
    ExecutionMode, ExecutionPlan, PayloadSpec, RouteEntry, RouteHandler, ValidationRules,
    WebSocketEntry,
};
use fastrapi_routing::security::describe_scheme;
use hyper::StatusCode;
use pyo3::Bound;
use pyo3::exceptions::PyAssertionError;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyTuple};
use smallvec::SmallVec;
use std::num::NonZeroU32;
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};

#[inline]
fn extract<'py, T>(kwargs: Option<&Bound<'py, PyDict>>, key: &str) -> Option<T>
where
    T: for<'a> pyo3::FromPyObject<'a, 'py>,
{
    let kw = kwargs?;
    let obj = kw.get_item(key).ok()??;
    obj.extract().ok()
}

pub(crate) fn security_requirements(
    py: Python<'_>,
    nodes: &[DependencyNode],
) -> Vec<fastrapi_core::security::RouteSecurityRequirement> {
    use fastrapi_core::security::{CompiledSecurityScheme, RouteSecurityRequirement};
    nodes
        .iter()
        .filter_map(|node| {
            let bound = node.func.bind(py);
            let spec = describe_scheme(py, bound)?;
            Some(RouteSecurityRequirement {
                scheme: Arc::new(CompiledSecurityScheme {
                    id: bound.as_ptr() as u32,
                    name: spec.name,
                    description: spec.description,
                    scopes: spec.scopes,
                    kind: spec.kind,
                }),
                scopes: node.scopes.clone(),
            })
        })
        .collect()
}

fn generator_stream_factory(
    py: Python<'_>,
    func: &Bound<'_, PyAny>,
    response_class: Option<&Py<PyAny>>,
    dump_options: Option<&Py<PyDict>>,
    status_code: Option<u16>,
) -> PyResult<Option<Py<PyAny>>> {
    let inspect = py.import("inspect")?;
    let is_generator = inspect
        .call_method1("isgeneratorfunction", (func,))?
        .is_truthy()?
        || inspect
            .call_method1("isasyncgenfunction", (func,))?
            .is_truthy()?;
    if !is_generator {
        return Ok(None);
    }
    let mode = match response_class {
        None => "jsonl",
        Some(class)
            if class
                .bind(py)
                .cast::<pyo3::types::PyType>()
                .is_ok_and(|class| {
                    class
                        .is_subclass_of::<fastrapi_http::sse::PyEventSourceResponse>()
                        .unwrap_or(false)
                }) =>
        {
            "sse"
        }
        Some(_) => "raw",
    };
    let typing = py.import("typing")?;
    let item_type = typing
        .getattr("get_type_hints")?
        .call1((func,))
        .ok()
        .and_then(|hints| hints.get_item("return").ok())
        .and_then(|annotation| {
            let args = typing
                .getattr("get_args")
                .ok()?
                .call1((&annotation,))
                .ok()?;
            args.get_item(0).ok()
        })
        .filter(|item| {
            !item.is_none()
                && item
                    .getattr("__name__")
                    .and_then(|name| name.extract::<String>())
                    .map_or(true, |name| name != "ServerSentEvent")
        });
    fastrapi_http::response_convert::stream_factory(
        py,
        mode,
        item_type.as_ref(),
        response_class.map(|class| class.bind(py)),
        dump_options.map(|options| options.bind(py)),
        status_code,
    )
    .map(Some)
}

/// Checks `responses=` as FastAPI does when a route is defined: every entry is a dict,
/// and its `model` is a type pydantic can build a field for.
fn check_additional_responses(py: Python<'_>, responses: Option<&Py<PyAny>>) -> PyResult<()> {
    let Some(responses) = responses else {
        return Ok(());
    };
    for (_, response) in responses.bind(py).cast::<PyDict>()?.iter() {
        let response = response
            .cast_into::<PyDict>()
            .map_err(|_| PyAssertionError::new_err("An additional response must be a dict"))?;
        if let Some(model) = response.get_item("model")?
            && model.is_truthy()?
        {
            response_type_adapter(&model)?;
        }
    }
    Ok(())
}

fn inferred_response_model(py: Python<'_>, func: &Bound<'_, PyAny>) -> Option<Py<PyAny>> {
    let typing = py.import("typing").ok()?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("include_extras", true).ok()?;
    let annotation = typing
        .getattr("get_type_hints")
        .ok()?
        .call((func,), Some(&kwargs))
        .ok()
        .and_then(|hints| hints.get_item("return").ok())
        .or_else(|| {
            func.getattr("__annotations__")
                .ok()?
                .get_item("return")
                .ok()
                .filter(|annotation| !annotation.is_instance_of::<pyo3::types::PyString>())
        })?;
    if annotation.is_none() || annotation.is(&typing.getattr("Any").ok()?) {
        return None;
    }
    let builtins = py.import("builtins").ok()?;
    let none_type = builtins.getattr("type").ok()?.call1((py.None(),)).ok()?;
    if annotation.is(&none_type) {
        return None;
    }
    for name in fastrapi_openapi::schema::BARE_BUILTIN_RETURNS {
        if annotation.is(&builtins.getattr(name).ok()?) {
            return None;
        }
    }
    let is_response_class = |candidate: &Bound<'_, PyAny>| {
        candidate.cast::<pyo3::types::PyType>().is_ok_and(|class| {
            class
                .is_subclass_of::<fastrapi_http::responses::PyResponse>()
                .unwrap_or(false)
                || py
                    .import("starlette.responses")
                    .and_then(|module| module.getattr("Response"))
                    .is_ok_and(|base| class.is_subclass(&base).unwrap_or(false))
        })
    };
    if is_response_class(&annotation) {
        return None;
    }
    let args = typing
        .getattr("get_args")
        .ok()?
        .call1((&annotation,))
        .ok()?;
    if args
        .try_iter()
        .ok()?
        .filter_map(Result::ok)
        .any(|arg| is_response_class(&arg))
    {
        return None;
    }
    let origin = typing
        .getattr("get_origin")
        .ok()?
        .call1((&annotation,))
        .ok()?;
    if !origin.is_none() {
        let abc = py.import("collections.abc").ok()?;
        for name in [
            "Iterator",
            "AsyncIterator",
            "Generator",
            "AsyncGenerator",
            "Iterable",
            "AsyncIterable",
        ] {
            if origin.is(&abc.getattr(name).ok()?) {
                return None;
            }
        }
    }
    Some(annotation.unbind())
}

/// The name of a tag: a `str`, or the value of an `Enum` member, as FastAPI's `tags: list[str | Enum]` allows.
pub(crate) fn tag_name(tag: &Bound<'_, PyAny>) -> Option<String> {
    tag.extract()
        .ok()
        .or_else(|| tag.getattr("value").ok()?.extract().ok())
}

/// Starlette asserts that every routed path starts with `/`.
pub(crate) fn ensure_routed_path(path: &str) -> PyResult<()> {
    if !path.starts_with('/') {
        return Err(PyAssertionError::new_err(
            "Routed paths must start with '/'",
        ));
    }
    ensure_path_template(path)
}

/// Starlette's `compile_path` rejects an unknown convertor and a parameter name that appears twice.
pub(crate) fn ensure_path_template(path: &str) -> PyResult<()> {
    if let Some(kind) = extract_path_convertors(path)
        .iter()
        .find(|kind| !matches!(kind.as_str(), "str" | "path" | "int" | "float" | "uuid"))
    {
        return Err(PyAssertionError::new_err(format!(
            "Unknown path convertor '{kind}'"
        )));
    }
    let names = extract_path_param_names(path);
    let mut repeated: Vec<&str> = (0..names.len())
        .filter(|&i| names[..i].contains(&names[i]))
        .map(|i| names[i].as_str())
        .collect();
    repeated.sort_unstable();
    repeated.dedup();
    if repeated.is_empty() {
        return Ok(());
    }
    let ending = if repeated.len() > 1 { "s" } else { "" };
    Err(PyValueError::new_err(format!(
        "Duplicated param name{ending} {} at path {path}",
        repeated.join(", ")
    )))
}

pub(crate) const ADD_API_ROUTE_OPTIONS: [&str; 26] = [
    "response_model",
    "status_code",
    "tags",
    "dependencies",
    "summary",
    "description",
    "response_description",
    "responses",
    "deprecated",
    "operation_id",
    "response_model_include",
    "response_model_exclude",
    "response_model_by_alias",
    "response_model_exclude_unset",
    "response_model_exclude_defaults",
    "response_model_exclude_none",
    "include_in_schema",
    "response_class",
    "name",
    "route_class_override",
    "callbacks",
    "openapi_extra",
    "generate_unique_id_function",
    "strict_content_type",
    "cache_resp",
    "rate_limit",
];

/// Raises the `TypeError` Python gives when a route decorator such as `FastAPI.get` receives an option it does not have.
pub(crate) fn ensure_route_options(
    callable: &str,
    kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<()> {
    for key in kwargs.into_iter().flat_map(PyDictMethods::keys) {
        let name: String = key.extract()?;
        if !ADD_API_ROUTE_OPTIONS.contains(&name.as_str()) {
            return Err(pyo3::exceptions::PyTypeError::new_err(format!(
                "{callable}() got an unexpected keyword argument '{name}'"
            )));
        }
    }
    Ok(())
}

pub(crate) fn add_api_route_kwargs<'py>(
    py: Python<'py>,
    kwargs: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyDict>> {
    let route_kwargs = PyDict::new(py);
    let Some(kwargs) = kwargs else {
        return Ok(route_kwargs);
    };
    ensure_route_options("add_api_route", Some(kwargs))?;
    for (key, value) in kwargs.iter() {
        let name: String = key.extract()?;
        if !value.is_none() || name == "response_model" {
            route_kwargs.set_item(key, value)?;
        }
    }
    Ok(route_kwargs)
}

#[inline]
fn extract_py(kwargs: Option<&Bound<'_, PyDict>>, key: &str) -> Option<Py<PyAny>> {
    let kw = kwargs?;
    let obj = kw.get_item(key).ok()??;
    Some(obj.unbind())
}

#[inline]
fn extract_bound<'py>(kwargs: Option<&Bound<'py, PyDict>>, key: &str) -> Option<Bound<'py, PyAny>> {
    let kw = kwargs?;
    kw.get_item(key).ok()?
}

#[inline]
fn func_name(func: &Bound<'_, PyAny>) -> String {
    func.getattr("__name__")
        .or_else(|_| func.get_type().getattr("__name__"))
        .and_then(|n| n.extract::<String>())
        .unwrap_or_default()
}

struct RouteOptions {
    name: Option<String>,
    generate_unique_id_function: Option<Py<PyAny>>,
    dependencies: Option<Py<PyAny>>,
    default_status: Option<StatusCode>,
    response_model: Option<Py<PyAny>>,
    response_class: Option<Py<PyAny>>,
    summary: Option<String>,
    description: Option<String>,
    include_in_schema: bool,
    strict_content_type: Option<bool>,
    cache_resp: bool,
    rate_limit: Option<NonZeroU32>,
    response_description: Option<String>,
    operation_id: Option<String>,
    responses: Option<Py<PyAny>>,
    openapi_extra: Option<simd_json::OwnedValue>,
    callbacks: Option<simd_json::OwnedValue>,
    callback_routes: Option<Py<PyAny>>,
    bypass_serialization: bool,
    merged_tags: Vec<String>,
    resolved_deprecated: Option<bool>,
    dump_options: Option<Py<PyDict>>,
}

impl RouteOptions {
    fn from_kwargs(
        py: Python<'_>,
        kwargs: Option<&Bound<'_, PyDict>>,
        parent_tags: &[String],
        parent_deprecated: Option<bool>,
    ) -> PyResult<Self> {
        let name = extract::<String>(kwargs, "name");
        let generate_unique_id_function = extract_bound(kwargs, "generate_unique_id_function")
            .filter(|function| !function.is_none())
            .map(Bound::unbind);
        let dependencies = extract_py(kwargs, "dependencies");
        let status_code = extract::<u16>(kwargs, "status_code");
        let summary = extract::<String>(kwargs, "summary");
        let description = extract::<String>(kwargs, "description");
        let include_in_schema = extract::<bool>(kwargs, "include_in_schema").unwrap_or(true);
        let strict_content_type = extract::<bool>(kwargs, "strict_content_type");
        let cache_resp = extract::<bool>(kwargs, "cache_resp").unwrap_or(false);
        let rate_limit = extract::<u32>(kwargs, "rate_limit")
            .map(|rps| {
                NonZeroU32::new(rps)
                    .ok_or_else(|| PyValueError::new_err("rate_limit must be greater than 0"))
            })
            .transpose()?;
        let response_description = extract::<String>(kwargs, "response_description");
        let operation_id = extract::<String>(kwargs, "operation_id");
        let deprecated = extract::<bool>(kwargs, "deprecated");

        let response_model = extract_py(kwargs, "response_model");
        let response_class = extract_py(kwargs, "response_class");

        let default_status = status_code
            .map(|c| {
                StatusCode::from_u16(c)
                    .map_err(|_| PyValueError::new_err("status_code must be between 100 and 599"))
            })
            .transpose()?;

        let bypass_serialization = response_model
            .as_ref()
            .is_some_and(|rm| rm.bind(py).is_none());

        let dump_options = build_dump_options(py, kwargs)?;

        let responses = extract_bound(kwargs, "responses")
            .filter(|responses| !responses.is_none())
            .map(Bound::unbind);

        let openapi_extra = extract_bound(kwargs, "openapi_extra").map(|d| py_any_to_json(py, &d));

        let callback_routes = extract_bound(kwargs, "callbacks").filter(|routes| !routes.is_none());
        let callbacks = callback_routes
            .as_ref()
            .and_then(|x| parse_callbacks_to_json(py, x, super::router::collect_router_routes));
        let callback_routes = callback_routes.map(Bound::unbind);

        let mut merged_tags = parent_tags.to_vec();
        if let Some(kw) = kwargs
            && let Ok(Some(tags_ref)) = kw.get_item("tags")
            && let Ok(iter) = tags_ref.try_iter()
        {
            for item in iter.flatten() {
                if let Some(tag) = tag_name(&item)
                    && !merged_tags.contains(&tag)
                {
                    merged_tags.push(tag);
                }
            }
        }

        let resolved_deprecated = deprecated.filter(|&flag| flag).or(parent_deprecated);

        Ok(Self {
            name,
            generate_unique_id_function,
            dependencies,
            default_status,
            response_model,
            response_class,
            summary,
            description,
            include_in_schema,
            strict_content_type,
            cache_resp,
            rate_limit,
            response_description,
            operation_id,
            responses,
            openapi_extra,
            callbacks,
            callback_routes,
            bypass_serialization,
            merged_tags,
            resolved_deprecated,
            dump_options,
        })
    }
}
fn build_dump_options(
    py: Python<'_>,
    kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<Option<Py<PyDict>>> {
    let include = extract_py(kwargs, "response_model_include");
    let exclude = extract_py(kwargs, "response_model_exclude");
    let by_alias = extract::<bool>(kwargs, "response_model_by_alias").unwrap_or(true);
    let exclude_unset = extract::<bool>(kwargs, "response_model_exclude_unset").unwrap_or(false);
    let exclude_defaults =
        extract::<bool>(kwargs, "response_model_exclude_defaults").unwrap_or(false);
    let exclude_none = extract::<bool>(kwargs, "response_model_exclude_none").unwrap_or(false);

    if include.is_none()
        && exclude.is_none()
        && !by_alias
        && !exclude_unset
        && !exclude_defaults
        && !exclude_none
    {
        return Ok(None);
    }

    let options = PyDict::new(py);
    if let Some(inc) = include {
        options.set_item("include", inc)?;
    }
    if let Some(exc) = exclude {
        options.set_item("exclude", exc)?;
    }
    if by_alias {
        options.set_item("by_alias", true)?;
    }
    if exclude_unset {
        options.set_item("exclude_unset", true)?;
    }
    if exclude_defaults {
        options.set_item("exclude_defaults", true)?;
    }
    if exclude_none {
        options.set_item("exclude_none", true)?;
    }

    Ok(Some(options.unbind()))
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Route analysis tracks independent body and argument switches"
)]
struct RouteAnalysis {
    needs_kwargs: bool,
    body_param_indices: SmallVec<[usize; 4]>,
    request_param_indices: SmallVec<[usize; 8]>,
    body_embed: bool,
    body_is_form: bool,
    has_multiple_query_params: bool,
}

impl RouteAnalysis {
    fn new(metadata: &ParsedRouteMetadata) -> Self {
        let needs_kwargs = !metadata.body_param_names.is_empty()
            || !metadata.param_validators.is_empty()
            || !metadata.dependencies.is_empty()
            || !metadata.parsed_params.is_empty()
            || metadata.request_param.is_some()
            || metadata.response_param.is_some();

        let mut body_param_name_set = AHashSet::with_capacity(
            metadata.body_param_names.len() + metadata.param_validators.len(),
        );

        for p in &metadata.parsed_params {
            if matches!(p.source, ParameterSource::Body) {
                body_param_name_set.insert(p.name.clone());
            }
        }
        for v in &metadata.param_validators {
            body_param_name_set.insert(v.name.clone());
        }

        let mut body_param_indices: SmallVec<[usize; 4]> = SmallVec::new();
        let mut request_param_indices: SmallVec<[usize; 8]> = SmallVec::new();
        for (idx, param) in metadata.parsed_params.iter().enumerate() {
            if matches!(param.source, ParameterSource::Body)
                || body_param_name_set.contains(param.name.as_str())
            {
                body_param_indices.push(idx);
            } else {
                request_param_indices.push(idx);
            }
        }

        let body_params = || {
            body_param_indices
                .iter()
                .map(|&idx| &metadata.parsed_params[idx])
        };
        let body_is_form = body_params().any(|param| param.is_form);
        let body_embed = body_params().next().is_some_and(|first| {
            body_param_indices.len() > 1
                || first.embed
                || (first.is_form && !first.is_pydantic_model)
        });

        let has_multiple_query_params = metadata
            .parsed_params
            .iter()
            .filter(|p| matches!(p.source, ParameterSource::Query))
            .count()
            > 1;

        Self {
            needs_kwargs,
            body_param_indices,
            request_param_indices,
            body_embed,
            body_is_form,
            has_multiple_query_params,
        }
    }
}

impl PyAPIRouter {
    pub fn create_method_decorator_kw(
        &self,
        py: Python<'_>,
        method: HttpMethod,
        path: String,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        self.ensure_mutable()?;
        ensure_path_template(&path)?;

        let router_tags = self.tags.clone();
        let merged_kwargs;
        let kwargs = match &self.callbacks {
            Some(router_callbacks) => {
                let copy = match kwargs {
                    Some(kwargs) => kwargs.copy()?,
                    None => PyDict::new(py),
                };
                let callbacks = pyo3::types::PyList::empty(py);
                for callback in router_callbacks.bind(py).try_iter()? {
                    callbacks.append(callback?)?;
                }
                if let Some(route_callbacks) = copy.get_item("callbacks")?
                    && !route_callbacks.is_none()
                {
                    for callback in route_callbacks.try_iter()? {
                        callbacks.append(callback?)?;
                    }
                }
                copy.set_item("callbacks", callbacks)?;
                merged_kwargs = copy;
                Some(&merged_kwargs)
            }
            None => kwargs,
        };
        let opts = RouteOptions::from_kwargs(py, kwargs, &router_tags, self.deprecated)?;
        let router_include_in_schema = self.include_in_schema;
        let router_default_class = self
            .default_response_class
            .as_ref()
            .map(|class| class.clone_ref(py));
        let path_for_closure = path;
        let routes = Arc::clone(&self.route_entries);
        let frozen = Arc::clone(&self.frozen);
        let revision = Arc::clone(&self.revision);
        let router_route_class = extract_bound(kwargs, "route_class_override")
            .filter(|class| !class.is_none())
            .map(Bound::unbind)
            .or_else(|| self.route_class.clone());
        let router_unique_id_fn = self.generate_unique_id_function.clone();

        let decorator = move |args: &Bound<'_, PyTuple>,
                              _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> {
            let py = args.py();
            let func: Bound<'_, PyAny> = args.get_item(0)?;

            let mut metadata = parse_route_metadata(py, &func, &path_for_closure)?;
            if let Some(dependencies) = &opts.dependencies {
                let path_param_names = extract_path_param_names(&path_for_closure);
                let markers = collect_dependency_markers(py, Some(dependencies));
                let external = parse_external_dependencies(py, &markers, &path_param_names)?;
                metadata.dependencies = merge_dependencies(external, metadata.dependencies);
            }
            let analysis = RouteAnalysis::new(&metadata);

            let security_requirements = security_requirements(py, &metadata.dependencies);

            let effective_response_class = opts
                .response_class
                .clone()
                .or_else(|| {
                    router_route_class.as_ref().and_then(|rc| {
                        rc.bind(py)
                            .getattr("response_class")
                            .ok()
                            .filter(|c| !c.is_none())
                            .map(Bound::unbind)
                    })
                })
                .or_else(|| {
                    router_default_class
                        .as_ref()
                        .map(|class| class.clone_ref(py))
                });
            let explicit_response_class = effective_response_class.is_some();
            let stream_factory = generator_stream_factory(
                py,
                &func,
                effective_response_class.as_ref(),
                opts.dump_options.as_ref(),
                opts.default_status.map(|status| status.as_u16()),
            )?;

            let final_response_type = effective_response_class
                .as_ref()
                .map_or(metadata.response_type, |class| {
                    super::tree::response_class_type(py, class.bind(py))
                });

            let path_param_names: Vec<Arc<str>> = extract_path_param_names(&path_for_closure)
                .into_iter()
                .map(|s| Arc::from(s.as_str()))
                .collect();

            let response_model = opts
                .response_model
                .as_ref()
                .map(|model| model.clone_ref(py))
                .or_else(|| inferred_response_model(py, &func));
            if let Some(status) = opts.default_status
                && !opts.bypass_serialization
                && response_model.is_some()
                && !fastrapi_http::status::body_allowed(status.as_u16())
            {
                return Err(PyAssertionError::new_err(format!(
                    "Status code {} must not have a response body",
                    status.as_u16()
                )));
            }
            let serialization_hint = if opts.bypass_serialization {
                SerializationHint::Unknown
            } else if response_model.is_some() {
                SerializationHint::PydanticModel
            } else {
                metadata.serialization_hint
            };

            let mut handler = RouteHandler {
                execution: ExecutionPlan {
                    func: func.clone().unbind(),
                    call_layout: CallLayout::of(&func).map(Arc::new),
                    is_async: metadata.is_async,
                    inline_async: metadata.inline_async,
                    is_fast_path: metadata.is_fast_path,
                    execution_mode: ExecutionMode::SyncNoArgs,
                    cache_response: opts.cache_resp,
                    rate_limit_per_second: opts.rate_limit,
                    custom_handler: OnceLock::new(),
                    operation_spans: false,
                },
                payload: PayloadSpec {
                    dependency_needs_request: false,
                    all_deps_sync: true,
                    dependency_needs_response: false,
                    has_generators: false,
                    has_async_dependencies: false,
                    needs_kwargs: analysis.needs_kwargs,
                    request_param: metadata.request_param.clone(),
                    response_param: metadata.response_param.clone(),
                    dependency_overrides: None,
                    body_param_names: metadata.body_param_names,
                    body_param_indices: analysis.body_param_indices,
                    request_param_indices: analysis.request_param_indices,
                    body_embed: analysis.body_embed,
                    body_is_form: analysis.body_is_form,
                    strict_content_type: opts.strict_content_type,
                    needs_body: false,
                    override_cache: OverrideCache::default(),
                    dependencies: metadata.dependencies,
                    parsed_params: metadata.parsed_params,
                    has_multiple_query_params: analysis.has_multiple_query_params,
                    path_param_names,
                },
                validation: ValidationRules {
                    param_validators: metadata.param_validators,
                    bypass_serialization: opts.bypass_serialization,
                },
                response: ResponseFormatter {
                    response_type: final_response_type,
                    serialization_hint,
                    default_status: opts.default_status,
                    response_model: response_model.as_ref().map(|model| model.clone_ref(py)),
                    response_class: effective_response_class,
                    dump_options: opts.dump_options.clone(),
                    response_adapter: Arc::new(pyo3::sync::PyOnceLock::new()),
                    stream_factory,
                },
            };

            handler.payload.refresh_dependency_flags();
            handler.assign_execution_mode();
            prepare_response_model(py, &handler.response)?;
            check_additional_responses(py, opts.responses.as_ref())?;

            let route_name = opts.name.clone().unwrap_or_else(|| func_name(&func));
            let route_instance = router_route_class
                .as_ref()
                .map(|rc| -> PyResult<Py<PyAny>> {
                    let to_py = |v: Option<&simd_json::OwnedValue>| {
                        v.map(|value| json_to_py_object(py, value))
                    };
                    let call_kwargs = PyDict::new(py);
                    call_kwargs.set_item("name", &route_name)?;
                    call_kwargs.set_item("response_model", response_model.as_ref())?;
                    call_kwargs.set_item("status_code", opts.default_status.map(|s| s.as_u16()))?;
                    call_kwargs.set_item("tags", opts.merged_tags.clone())?;
                    call_kwargs.set_item("summary", opts.summary.clone())?;
                    call_kwargs.set_item("description", opts.description.clone())?;
                    call_kwargs.set_item(
                        "response_description",
                        opts.response_description
                            .clone()
                            .unwrap_or_else(|| "Successful Response".to_string()),
                    )?;
                    call_kwargs.set_item("responses", opts.responses.as_ref())?;
                    call_kwargs.set_item("deprecated", opts.resolved_deprecated)?;
                    call_kwargs.set_item("operation_id", opts.operation_id.clone())?;
                    call_kwargs.set_item("include_in_schema", opts.include_in_schema)?;
                    call_kwargs.set_item("response_class", opts.response_class.as_ref())?;
                    call_kwargs.set_item("openapi_extra", to_py(opts.openapi_extra.as_ref()))?;
                    call_kwargs
                        .set_item("generate_unique_id_function", router_unique_id_fn.as_ref())?;
                    call_kwargs.set_item("callbacks", to_py(opts.callbacks.as_ref()))?;
                    let class = rc.bind(py);
                    let instance = class.call1((path_for_closure.clone(), func.clone()))?;
                    for (key, value) in call_kwargs.iter() {
                        let key_str: String = key.extract()?;
                        if instance
                            .getattr(key_str.as_str())
                            .is_ok_and(|current| current.is(&value))
                        {
                            continue;
                        }
                        instance.setattr(key_str.as_str(), &value)?;
                    }
                    Ok(instance.unbind())
                })
                .transpose()?;

            let (
                extra_summary,
                extra_description,
                extra_operation_id,
                extra_openapi,
                extra_include,
            ) = route_instance
                .as_ref()
                .map(|inst| {
                    let bound = inst.bind(py);
                    (
                        bound
                            .getattr("summary")
                            .ok()
                            .and_then(|v| v.extract::<String>().ok()),
                        bound
                            .getattr("description")
                            .ok()
                            .and_then(|v| v.extract::<String>().ok()),
                        bound
                            .getattr("operation_id")
                            .ok()
                            .and_then(|v| v.extract::<String>().ok()),
                        bound
                            .getattr("openapi_extra")
                            .ok()
                            .filter(|v| !v.is_none())
                            .map(|v| py_any_to_json(py, &v)),
                        bound
                            .getattr("include_in_schema")
                            .ok()
                            .and_then(|v| v.extract::<bool>().ok()),
                    )
                })
                .unwrap_or_default();

            super::tree::ensure_unfrozen(&frozen)?;
            revision.fetch_add(1, Ordering::Release);
            routes.lock().push(RouteEntry {
                method,
                path: path_for_closure.clone(),
                name: route_name,
                explicit_response_class,
                route_object: Arc::new(parking_lot::Mutex::new(None)),
                generate_unique_id_function: opts
                    .generate_unique_id_function
                    .as_ref()
                    .or(router_unique_id_fn.as_ref())
                    .map(|function| function.clone_ref(py)),
                handler: Arc::new(handler),
                route_instance,
                tags: opts.merged_tags.clone(),
                summary: opts.summary.clone().or(extra_summary),
                description: endpoint_description(
                    py,
                    opts.description.as_deref().or(extra_description.as_deref()),
                    &func,
                ),
                response_description: opts.response_description.clone(),
                operation_id: opts.operation_id.clone().or(extra_operation_id),
                responses: opts
                    .responses
                    .as_ref()
                    .map(|responses| responses.clone_ref(py)),
                openapi_extra: opts.openapi_extra.clone().or(extra_openapi),
                callbacks: opts.callbacks.clone(),
                dependency_markers: opts
                    .dependencies
                    .as_ref()
                    .map(|markers| markers.clone_ref(py)),
                callback_routes: opts
                    .callback_routes
                    .as_ref()
                    .map(|routes| routes.clone_ref(py)),
                deprecated: opts.resolved_deprecated,
                include_in_schema: extra_include.unwrap_or(opts.include_in_schema)
                    && router_include_in_schema,
                security: security_requirements,
                host: None,
            });

            Ok(func.unbind())
        };

        PyCFunction::new_closure(py, None, None, decorator).map(Into::into)
    }

    pub fn create_ws_decorator(
        &self,
        py: Python<'_>,
        path: String,
        name: Option<String>,
        dependencies: Option<Py<PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        self.ensure_mutable()?;
        if !path.starts_with('/') {
            return Err(PyValueError::new_err("WebSocket path must start with '/'"));
        }
        let websockets = Arc::clone(&self.websocket_entries);
        let frozen = Arc::clone(&self.frozen);
        let revision = Arc::clone(&self.revision);
        let closure = move |args: &Bound<'_, PyTuple>,
                            _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> {
            let py = args.py();
            let func: Py<PyAny> = args.get_item(0)?.unbind();

            let path_param_names = extract_path_param_names(&path);
            super::tree::ensure_unfrozen(&frozen)?;
            let mut nodes = parse_dependencies(py, func.bind(py), &path_param_names)?;
            if let Some(dependencies) = &dependencies {
                let markers = collect_dependency_markers(py, Some(dependencies));
                let external = parse_external_dependencies(py, &markers, &path_param_names)?;
                nodes = merge_dependencies(external, nodes);
            }
            let deps: SmallVec<[DependencyNode; 4]> = nodes.into();
            let signature = parse_websocket_signature(py, func.bind(py), &path, &deps)?;

            let entry = WebSocketEntry {
                path: path.clone(),
                name: name.clone().unwrap_or_else(|| func_name(func.bind(py))),
                handler: func.clone_ref(py),
                websocket_arg: signature.websocket_arg,
                params: signature.params,
                deps,
                dependency_overrides: None,
                host: None,
            };
            websockets.lock().push(entry);
            revision.fetch_add(1, Ordering::Release);
            Ok(func)
        };
        PyCFunction::new_closure(py, None, None, closure).map(Into::into)
    }

    pub(crate) fn create_api_route_decorator(
        &self,
        py: Python<'_>,
        path: &str,
        methods: Option<Vec<String>>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let decorators = methods
            .unwrap_or_else(|| vec!["GET".to_owned()])
            .into_iter()
            .map(|method| {
                let http_method = HttpMethod::from_str(&method.to_ascii_uppercase())
                    .map_err(|_| PyValueError::new_err(format!("Invalid HTTP method: {method}")))?;
                self.create_method_decorator_kw(py, http_method, path.to_owned(), kwargs)
            })
            .collect::<PyResult<Vec<_>>>()?;
        let closure = move |args: &Bound<'_, PyTuple>,
                            _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> {
            let func = args.get_item(0)?;
            if decorators.is_empty() {
                return Err(PyAssertionError::new_err(()));
            }
            for decorator in &decorators {
                decorator.bind(args.py()).call1((&func,))?;
            }
            Ok(func.unbind())
        };
        PyCFunction::new_closure(py, None, None, closure).map(Into::into)
    }

    pub(crate) fn add_starlette_route(
        &self,
        py: Python<'_>,
        path: &str,
        endpoint: &Bound<'_, PyAny>,
        methods: Option<Vec<String>>,
        name: Option<String>,
        _include_in_schema: bool,
    ) -> PyResult<()> {
        let mut methods = methods.unwrap_or_else(|| vec!["GET".to_owned()]);
        // Starlette's `Route` answers HEAD wherever it allows GET.
        if methods
            .iter()
            .any(|method| method.eq_ignore_ascii_case("GET"))
            && !methods
                .iter()
                .any(|method| method.eq_ignore_ascii_case("HEAD"))
        {
            methods.push("HEAD".to_owned());
        }
        let wrapped = crate::ffi::compat::py_helpers(py)?
            .getattr("request_endpoint")?
            .call1((endpoint,))?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("name", name.unwrap_or_else(|| func_name(endpoint)))?;
        kwargs.set_item("include_in_schema", false)?;
        self.add_api_route_impl(py, path, &wrapped, Some(methods), &kwargs)
    }

    pub(crate) fn add_api_route_impl(
        &self,
        py: Python<'_>,
        path: &str,
        endpoint: &Bound<'_, PyAny>,
        methods: Option<Vec<String>>,
        kwargs: &Bound<'_, PyDict>,
    ) -> PyResult<()> {
        let methods = methods.unwrap_or_else(|| vec!["GET".to_string()]);
        if methods.is_empty() {
            return Err(PyAssertionError::new_err(()));
        }
        for method in methods {
            let http_method = HttpMethod::from_str(&method.to_ascii_uppercase())
                .map_err(|_| PyValueError::new_err(format!("Invalid HTTP method: {method}")))?;
            let decorator =
                self.create_method_decorator_kw(py, http_method, path.to_owned(), Some(kwargs))?;
            decorator.call1(py, (endpoint.clone(),))?;
        }
        Ok(())
    }
}
