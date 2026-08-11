use super::PyAPIRouter;
use crate::routing::types::{HttpMethod, ParameterSource, SerializationHint, WebSocketEntry};
use crate::utils::LockExt;
use ahash::AHashSet;
use hyper::StatusCode;
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyTuple};
use smallvec::SmallVec;
use std::sync::Arc;
use std::sync::atomic::Ordering;

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

struct RouteOptions {
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
    responses: Option<sonic_rs::Value>,
    openapi_extra: Option<sonic_rs::Value>,
    callbacks: Option<sonic_rs::Value>,
    bypass_serialization: bool,
    merged_tags: Vec<String>,
    resolved_deprecated: Option<bool>,
}

impl RouteOptions {
    fn from_kwargs(
        py: Python<'_>,
        kwargs: Option<&Bound<'_, PyDict>>,
        parent_tags: &[String],
        parent_deprecated: Option<bool>,
    ) -> PyResult<Self> {
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
                    pyo3::exceptions::PyValueError::new_err(
                        "status_code must be between 100 and 599",
                    )
                })
            })
            .transpose()?;

        let bypass_serialization = response_model
            .as_ref()
            .is_some_and(|rm| rm.bind(py).is_none());

        let responses =
            extract_bound(kwargs, "responses").map(|d| crate::utils::py_any_to_json(py, &d));

        let openapi_extra =
            extract_bound(kwargs, "openapi_extra").map(|d| crate::utils::py_any_to_json(py, &d));

        let callbacks = extract_bound(kwargs, "callbacks")
            .and_then(|x| crate::utils::openapi::parse_callbacks_to_json(py, &x));

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
        })
    }
}

// ==========================================
// 3. ROUTE ANALYSIS
// ==========================================

struct RouteAnalysis {
    needs_kwargs: bool,
    body_param_name_set: AHashSet<String>,
    body_param_indices: SmallVec<[usize; 4]>,
    defer_json_parse: bool,
    has_multiple_query_params: bool,
}

impl RouteAnalysis {
    fn new(metadata: &crate::ffi::pydantic::ParsedRouteMetadata) -> Self {
        let needs_kwargs = !metadata.body_param_names.is_empty()
            || !metadata.param_validators.is_empty()
            || !metadata.dependencies.is_empty()
            || !metadata.parsed_params.is_empty();

        let mut body_param_name_set = AHashSet::with_capacity(
            metadata.body_param_names.len() + metadata.param_validators.len(),
        );

        for p in metadata.parsed_params.iter() {
            if matches!(p.source, ParameterSource::Body) {
                body_param_name_set.insert(p.name.clone());
            }
        }
        for v in metadata.param_validators.iter() {
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
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "Cannot modify router after it has been frozen",
            ));
        }

        let opts = RouteOptions::from_kwargs(py, kwargs, &self.tags, self.deprecated)?;
        let path_for_closure = path;
        let routes = Arc::clone(&self.route_entries);

        let decorator = move |args: &Bound<'_, PyTuple>,
                              _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> {
            let py = args.py();
            let func: Bound<'_, PyAny> = args.get_item(0)?;

            let metadata = crate::ffi::pydantic::parse_route_metadata(py, &func, &path_for_closure);
            let analysis = RouteAnalysis::new(&metadata);

            let final_response_type = if let Some(cls) = &opts.response_class {
                crate::ffi::pydantic::get_response_type_from_class(py, cls.bind(py))
            } else {
                metadata.response_type
            };

            let path_param_names: Vec<Arc<str>> =
                crate::routing::params::extract_path_param_names(&path_for_closure)
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

            let mut handler = crate::routing::types::RouteHandler {
                execution: crate::routing::types::ExecutionPlan {
                    func: func.clone().unbind(),
                    is_async: metadata.is_async,
                    is_fast_path: metadata.is_fast_path,
                    execution_mode: crate::ffi::py_handlers::ExecutionMode::SyncNoArgs,
                    cache_response: opts.cache_resp,
                    rate_limit_per_second: opts.rate_limit,
                },
                payload: crate::routing::types::PayloadSpec {
                    dependency_needs_request: metadata.dependency_needs_request,
                    all_deps_sync: metadata.all_deps_sync,
                    needs_kwargs: analysis.needs_kwargs,
                    body_param_names: metadata.body_param_names,
                    body_param_name_set: analysis.body_param_name_set,
                    body_param_indices: analysis.body_param_indices,
                    dependencies: metadata.dependencies,
                    parsed_params: metadata.parsed_params,
                    has_multiple_query_params: analysis.has_multiple_query_params,
                    path_param_names,
                },
                validation: crate::routing::types::ValidationRules {
                    param_validators: metadata.param_validators,
                    defer_json_parse: analysis.defer_json_parse,
                    bypass_serialization: opts.bypass_serialization,
                },
                response: crate::routing::types::ResponseFormatter {
                    response_type: final_response_type,
                    serialization_hint,
                    default_status: opts.default_status,
                    response_model: opts.response_model.clone(),
                    response_class: opts.response_class.clone(),
                },
            };

            crate::ffi::py_handlers::assign_execution_mode(&mut handler);

            routes
                .lock_or_panic()
                .push(crate::routing::types::RouteEntry {
                    method,
                    path: path_for_closure.clone(),
                    handler: Arc::new(handler),
                    tags: opts.merged_tags.clone(),
                    summary: opts.summary.clone(),
                    description: opts.description.clone(),
                    response_description: opts.response_description.clone(),
                    operation_id: opts.operation_id.clone(),
                    responses: opts.responses.clone(),
                    openapi_extra: opts.openapi_extra.clone(),
                    callbacks: opts.callbacks.clone(),
                    deprecated: opts.resolved_deprecated,
                    include_in_schema: opts.include_in_schema,
                });

            Ok(func.unbind())
        };

        PyCFunction::new_closure(py, None, None, decorator).map(|f| f.into())
    }

    pub fn create_ws_decorator(&self, py: Python<'_>, path: String) -> PyResult<Py<PyAny>> {
        if self.frozen.load(Ordering::Relaxed) {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "Cannot modify router after it has been frozen",
            ));
        }
        if !path.starts_with('/') {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "WebSocket path must start with '/'",
            ));
        }
        let websockets = Arc::clone(&self.websocket_entries);
        let closure = move |args: &Bound<'_, PyTuple>,
                            _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> {
            let py = args.py();
            let func: Py<PyAny> = args.get_item(0)?.unbind();
            let entry = WebSocketEntry {
                path: path.clone(),
                handler: func.clone_ref(py),
            };
            websockets.lock_or_panic().push(entry);
            Ok(func)
        };
        PyCFunction::new_closure(py, None, None, closure).map(|f| f.into())
    }
}
