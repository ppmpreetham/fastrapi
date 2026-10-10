use super::PyAPIRouter;
use crate::routing::dependencies::{DependencyNode, InjectionType};
use crate::routing::types::{HttpMethod, ParameterSource, SerializationHint, WebSocketEntry};
use ahash::AHashSet;
use hyper::StatusCode;
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyTuple};
use smallvec::SmallVec;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use crate::ffi::py_handlers::ExecutionMode;
use crate::ffi::py_handlers::assign_execution_mode;
use crate::ffi::pydantic::ParsedRouteMetadata;
use crate::ffi::pydantic::get_response_type_from_class;
use crate::ffi::pydantic::parse_route_metadata;
use crate::routing::dependencies::collect_dependency_callables;
use crate::routing::dependencies::parse_dependencies;
use crate::routing::dependencies::parse_external_dependencies;
use crate::routing::dependencies::shift_dependency_indices;
use crate::routing::params::extract_path_param_names;
use crate::routing::security::describe_scheme;
use crate::routing::types::ExecutionPlan;
use crate::routing::types::PayloadSpec;
use crate::routing::types::ResponseFormatter;
use crate::routing::types::RouteEntry;
use crate::routing::types::RouteHandler;
use crate::routing::types::ValidationRules;
use crate::utils::json_to_py_object;
use crate::utils::openapi::parse_callbacks_to_json;
use crate::utils::py_any_to_json;
use pyo3::Bound;
use pyo3::exceptions::PyRuntimeError;
use pyo3::exceptions::PyValueError;
use std::convert::Into;

#[inline]
fn extract<'py, T>(kwargs: Option<&Bound<'py, PyDict>>, key: &str) -> Option<T>
where
    T: for<'a> pyo3::FromPyObject<'a, 'py>,
{
    let kw = kwargs?;
    let obj = kw.get_item(key).ok()??;
    obj.extract().ok()
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
fn func_name(_py: Python<'_>, func: &Bound<'_, PyAny>) -> String {
    func.getattr("__name__")
        .ok()
        .and_then(|n| n.extract::<String>().ok())
        .unwrap_or_default()
}

struct RouteOptions {
    dependencies: Option<Py<PyAny>>,
    default_status: Option<StatusCode>,
    response_model: Option<Py<PyAny>>,
    response_class: Option<Py<PyAny>>,
    summary: Option<String>,
    description: Option<String>,
    include_in_schema: bool,
    cache_resp: bool,
    rate_limit: Option<u32>,
    response_description: Option<String>,
    operation_id: Option<String>,
    responses: Option<simd_json::OwnedValue>,
    openapi_extra: Option<simd_json::OwnedValue>,
    callbacks: Option<simd_json::OwnedValue>,
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
        let dependencies = extract_py(kwargs, "dependencies");
        let status_code = extract::<u16>(kwargs, "status_code");
        let summary = extract::<String>(kwargs, "summary");
        let description = extract::<String>(kwargs, "description");
        let include_in_schema = extract::<bool>(kwargs, "include_in_schema").unwrap_or(true);
        let cache_resp = extract::<bool>(kwargs, "cache_resp").unwrap_or(false);
        let rate_limit = extract::<u32>(kwargs, "rate_limit");
        let response_description = extract::<String>(kwargs, "response_description");
        let operation_id = extract::<String>(kwargs, "operation_id");
        let deprecated = extract::<bool>(kwargs, "deprecated");

        let response_model = extract_py(kwargs, "response_model");
        let response_class = extract_py(kwargs, "response_class");

        let default_status = status_code
            .map(|c| {
                StatusCode::from_u16(c).map_err(|_| {
                    PyValueError::new_err(
                        "status_code must be between 100 and 599",
                    )
                })
            })
            .transpose()?;

        let bypass_serialization = response_model
            .as_ref()
            .is_some_and(|rm| rm.bind(py).is_none());

        let dump_options = build_dump_options(py, kwargs)?;

        let responses =
            extract_bound(kwargs, "responses").map(|d| py_any_to_json(py, &d));

        let openapi_extra =
            extract_bound(kwargs, "openapi_extra").map(|d| py_any_to_json(py, &d));

        let callbacks = extract_bound(kwargs, "callbacks")
            .and_then(|x| parse_callbacks_to_json(py, &x));

        let mut merged_tags = parent_tags.to_vec();
        if let Some(kw) = kwargs
            && let Ok(Some(tags_ref)) = kw.get_item("tags")
            && let Ok(iter) = tags_ref.try_iter()
        {
            for item in iter.flatten() {
                if let Ok(tag_slice) = item.extract::<&str>()
                    && !merged_tags.iter().any(|t| t == tag_slice)
                {
                    merged_tags.push(tag_slice.to_owned());
                }
            }
        }

        let resolved_deprecated = deprecated.or(parent_deprecated);

        Ok(Self {
            dependencies,
            default_status,
            response_model,
            response_class,
            summary,
            description,
            include_in_schema,
            cache_resp,
            rate_limit,
            response_description,
            operation_id,
            responses,
            openapi_extra,
            callbacks,
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

struct RouteAnalysis {
    needs_kwargs: bool,
    body_param_name_set: AHashSet<String>,
    body_param_indices: SmallVec<[usize; 4]>,
    defer_json_parse: bool,
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
        for (idx, param) in metadata.parsed_params.iter().enumerate() {
            if matches!(param.source, ParameterSource::Body)
                || body_param_name_set.contains(param.name.as_str())
            {
                body_param_indices.push(idx);
            }
        }

        let defer_json_parse = body_param_indices.len() == 1
            && metadata.parsed_params[body_param_indices[0]].is_pydantic_model;

        let has_multiple_query_params = metadata
            .parsed_params
            .iter()
            .filter(|p| matches!(p.source, ParameterSource::Query))
            .count()
            > 1;

        Self {
            needs_kwargs,
            body_param_name_set,
            body_param_indices,
            defer_json_parse,
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
        if self.frozen.load(Ordering::Relaxed) {
            return Err(PyRuntimeError::new_err(
                "Cannot modify router after it has been frozen",
            ));
        }

        let router_tags = self.tags.clone();
        let opts = RouteOptions::from_kwargs(py, kwargs, &router_tags, self.deprecated)?;
        let path_for_closure = path;
        let routes = Arc::clone(&self.route_entries);
        let router_route_class = self.route_class.clone();
        let router_unique_id_fn = self.generate_unique_id_function.clone();

        let decorator = move |args: &Bound<'_, PyTuple>,
                              _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> {
            let py = args.py();
            let func: Bound<'_, PyAny> = args.get_item(0)?;

            let mut metadata =
                parse_route_metadata(py, &func, &path_for_closure);
            if let Some(dependencies) = &opts.dependencies {
                let path_param_names =
                    extract_path_param_names(&path_for_closure);
                let callables = collect_dependency_callables(
                    py,
                    Some(dependencies),
                );
                let mut external = parse_external_dependencies(
                    py,
                    &callables,
                    &path_param_names,
                )?;
                shift_dependency_indices(
                    &mut metadata.dependencies,
                    external.len(),
                );
                external.append(&mut metadata.dependencies);
                metadata.dependencies = external;
                metadata.dependency_needs_request = metadata
                    .dependencies
                    .iter()
                    .any(|dependency| dependency.needs_request_object);
                metadata.all_deps_sync = metadata
                    .dependencies
                    .iter()
                    .all(DependencyNode::is_sync_callable);
            }
            let analysis = RouteAnalysis::new(&metadata);

            let mut security_requirements = Vec::new();
            for node in &metadata.dependencies {
                let bound = node.func.bind(py);
                if let Some(spec) = describe_scheme(py, bound) {
                    use crate::types::route::{CompiledSecurityScheme, RouteSecurityRequirement};
                    security_requirements.push(RouteSecurityRequirement {
                        scheme: Arc::new(CompiledSecurityScheme {
                            id: bound.as_ptr() as u32,
                            name: spec.name,
                            description: spec.description,
                            scopes: spec.scopes,
                            kind: spec.kind,
                        }),
                        scopes: node.scopes.clone().into(),
                    });
                }
            }

            let effective_response_class = opts.response_class.clone().or_else(|| {
                router_route_class.as_ref().and_then(|rc| {
                    rc.bind(py)
                        .getattr("response_class")
                        .ok()
                        .filter(|c| !c.is_none())
                        .map(Bound::unbind)
                })
            });

            let final_response_type = if let Some(cls) = &effective_response_class {
                get_response_type_from_class(py, cls.bind(py))
            } else {
                metadata.response_type
            };

            let path_param_names: Vec<Arc<str>> =
                extract_path_param_names(&path_for_closure)
                    .into_iter()
                    .map(|s| Arc::from(s.as_str()))
                    .collect();

            let serialization_hint = if opts.bypass_serialization {
                SerializationHint::Unknown
            } else if opts.response_model.is_some() {
                SerializationHint::PydanticModel
            } else {
                metadata.serialization_hint
            };

            let mut handler = RouteHandler {
                execution: ExecutionPlan {
                    func: func.clone().unbind(),
                    is_async: metadata.is_async,
                    is_fast_path: metadata.is_fast_path,
                    execution_mode: ExecutionMode::SyncNoArgs,
                    cache_response: opts.cache_resp,
                    rate_limit_per_second: opts.rate_limit,
                },
                payload: PayloadSpec {
                    dependency_needs_request: metadata.dependency_needs_request,
                    all_deps_sync: metadata.all_deps_sync,
                    needs_kwargs: analysis.needs_kwargs,
                    dependency_needs_response: metadata.dependencies.iter().any(|dependency| {
                        dependency
                            .injection_plan
                            .iter()
                            .any(|(_, injection)| matches!(injection, InjectionType::Response))
                    }),
                    has_generators: metadata
                        .dependencies
                        .iter()
                        .any(|dependency| dependency.is_generator || dependency.is_async_generator),
                    has_async_dependencies: metadata
                        .dependencies
                        .iter()
                        .any(|dependency| dependency.is_async || dependency.is_async_generator),
                    request_param: metadata.request_param.clone(),
                    response_param: metadata.response_param.clone(),
                    dependency_overrides: None,
                    body_param_names: metadata.body_param_names,
                    body_param_name_set: analysis.body_param_name_set,
                    body_param_indices: analysis.body_param_indices,
                    dependencies: metadata.dependencies,
                    parsed_params: metadata.parsed_params,
                    has_multiple_query_params: analysis.has_multiple_query_params,
                    path_param_names,
                },
                validation: ValidationRules {
                    param_validators: metadata.param_validators,
                    defer_json_parse: analysis.defer_json_parse,
                    bypass_serialization: opts.bypass_serialization,
                },
                response: ResponseFormatter {
                    response_type: final_response_type,
                    serialization_hint,
                    default_status: opts.default_status,
                    response_model: opts.response_model.clone(),
                    response_class: effective_response_class,
                    dump_options: opts.dump_options.clone(),
                },
            };

            assign_execution_mode(&mut handler);

            let route_instance = router_route_class
                .as_ref()
                .map(|rc| -> PyResult<Py<PyAny>> {
                    let to_py = |v: Option<&simd_json::OwnedValue>| {
                        v.map(|value| json_to_py_object(py, value))
                    };
                    let call_kwargs = PyDict::new(py);
                    call_kwargs.set_item("name", func_name(py, &func))?;
                    call_kwargs.set_item("response_model", opts.response_model.as_ref())?;
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
                    call_kwargs.set_item("responses", to_py(opts.responses.as_ref()))?;
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
                        let _ = instance.setattr(key_str.as_str(), &value);
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

            routes.lock().push(RouteEntry {
                method,
                path: path_for_closure.clone(),
                handler: Arc::new(handler),
                route_instance,
                tags: opts.merged_tags.clone(),
                summary: opts.summary.clone().or(extra_summary),
                description: opts.description.clone().or(extra_description),
                response_description: opts.response_description.clone(),
                operation_id: opts.operation_id.clone().or(extra_operation_id),
                responses: opts.responses.clone(),
                openapi_extra: opts.openapi_extra.clone().or(extra_openapi),
                callbacks: opts.callbacks.clone(),
                deprecated: opts.resolved_deprecated,
                include_in_schema: extra_include.unwrap_or(opts.include_in_schema),
                security: security_requirements,
                host: None,
            });

            Ok(func.unbind())
        };

        PyCFunction::new_closure(py, None, None, decorator).map(Into::into)
    }

    pub fn create_ws_decorator(&self, py: Python<'_>, path: String) -> PyResult<Py<PyAny>> {
        if self.frozen.load(Ordering::Relaxed) {
            return Err(PyRuntimeError::new_err(
                "Cannot modify router after it has been frozen",
            ));
        }
        if !path.starts_with('/') {
            return Err(PyValueError::new_err(
                "WebSocket path must start with '/'",
            ));
        }
        let websockets = Arc::clone(&self.websocket_entries);
        let closure = move |args: &Bound<'_, PyTuple>,
                            _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> {
            let py = args.py();
            let func: Py<PyAny> = args.get_item(0)?.unbind();

            let path_param_names = extract_path_param_names(&path);
            let deps: SmallVec<[DependencyNode; 4]> =
                parse_dependencies(
                    py,
                    func.bind(py),
                    &path_param_names,
                )
                .unwrap_or_default()
                .into();

            let entry = WebSocketEntry {
                path: path.clone(),
                handler: func.clone_ref(py),
                deps,
                dependency_overrides: None,
            };
            websockets.lock().push(entry);
            Ok(func)
        };
        PyCFunction::new_closure(py, None, None, closure).map(Into::into)
    }

    pub(crate) fn api_route_kwargs(
        py: Python<'_>,
        response_model: Option<Py<PyAny>>,
        status_code: Option<u16>,
        tags: Option<Py<PyAny>>,
        dependencies: Option<Py<PyAny>>,
        summary: Option<String>,
        description: Option<String>,
        response_description: Option<String>,
        responses: Option<Py<PyAny>>,
        deprecated: Option<bool>,
        operation_id: Option<String>,
        include_in_schema: bool,
        response_class: Option<Py<PyAny>>,
        name: Option<String>,
        callbacks: Option<Py<PyAny>>,
        openapi_extra: Option<Py<PyAny>>,
        generate_unique_id_function: Option<Py<PyAny>>,
    ) -> Bound<'_, PyDict> {
        let kwargs = PyDict::new(py);
        let entries: [(&str, Option<Bound<'_, PyAny>>); 16] = [
            ("response_model", response_model.map(|v| v.into_bound(py))),
            (
                "status_code",
                status_code.map(|v| v.into_pyobject(py).unwrap().into_any()),
            ),
            ("tags", tags.map(|v| v.into_bound(py))),
            ("dependencies", dependencies.map(|v| v.into_bound(py))),
            (
                "summary",
                summary.map(|v| v.into_pyobject(py).unwrap().into_any()),
            ),
            (
                "description",
                description.map(|v| v.into_pyobject(py).unwrap().into_any()),
            ),
            (
                "response_description",
                response_description.map(|v| v.into_pyobject(py).unwrap().into_any()),
            ),
            ("responses", responses.map(|v| v.into_bound(py))),
            (
                "deprecated",
                deprecated.map(|v| v.into_pyobject(py).unwrap().to_owned().into_any()),
            ),
            (
                "operation_id",
                operation_id.map(|v| v.into_pyobject(py).unwrap().into_any()),
            ),
            (
                "include_in_schema",
                Some(
                    include_in_schema
                        .into_pyobject(py)
                        .unwrap()
                        .to_owned()
                        .into_any(),
                ),
            ),
            ("response_class", response_class.map(|v| v.into_bound(py))),
            (
                "name",
                name.map(|v| v.into_pyobject(py).unwrap().into_any()),
            ),
            ("callbacks", callbacks.map(|v| v.into_bound(py))),
            ("openapi_extra", openapi_extra.map(|v| v.into_bound(py))),
            (
                "generate_unique_id_function",
                generate_unique_id_function.map(|v| v.into_bound(py)),
            ),
        ];
        for (key, value) in entries {
            if let Some(value) = value {
                kwargs.set_item(key, value).ok();
            }
        }
        kwargs
    }

    pub(crate) fn add_api_route_impl(
        &self,
        py: Python<'_>,
        path: String,
        endpoint: &Bound<'_, PyAny>,
        methods: Option<Vec<String>>,
        kwargs: &Bound<'_, PyDict>,
    ) -> PyResult<()> {
        for method in methods.unwrap_or_else(|| vec!["GET".to_string()]) {
            let http_method = HttpMethod::from_str(&method.to_ascii_uppercase()).map_err(|_| {
                PyValueError::new_err(format!("Invalid HTTP method: {method}"))
            })?;
            let decorator =
                self.create_method_decorator_kw(py, http_method, path.clone(), Some(kwargs))?;
            decorator.call1(py, (endpoint.clone(),))?;
        }
        Ok(())
    }
}
