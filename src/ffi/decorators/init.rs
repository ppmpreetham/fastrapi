use crate::routing::types::{
    HttpMethod, ParameterSource, RouteEntry, SerializationHint, WebSocketEntry,
};
use ahash::AHashSet;
use hyper::StatusCode;
use pyo3::prelude::{Bound, Py, PyAny, PyAnyMethods, PyResult, Python};
use pyo3::types::{PyCFunction, PyDict, PyTuple};
use pyo3::types::{PyString, PyStringMethods};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::PyAPIRouter;

impl PyAPIRouter {
    pub fn create_method_decorator(
        &self,
        py: Python<'_>,
        method: HttpMethod,
        path: String,
        status_code: Option<u16>,
        response_model: Option<Py<PyAny>>,
        response_class: Option<Py<PyAny>>,
        tags: Option<Py<PyAny>>,
        summary: Option<String>,
        description: Option<String>,
        deprecated: Option<bool>,
        include_in_schema: bool,
        cache_response: bool,
        rate_limit_per_second: Option<u32>,
    ) -> PyResult<Py<PyAny>> {
        if self.frozen.load(Ordering::Relaxed) {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "Cannot modify router after it has been frozen",
            ));
        }

        let default_status = status_code
            .map(|c| {
                StatusCode::from_u16(c).map_err(|_| {
                    pyo3::exceptions::PyValueError::new_err(
                        "status_code must be between 100 and 599",
                    )
                })
            })
            .transpose()?;

        let mut merged_tags = self.tags.clone();

        if let Some(route_tags) = tags {
            let tag_list = route_tags.bind(py);

            if let Ok(iter) = tag_list.try_iter() {
                iter.flatten().for_each(|item| {
                    if let Ok(py_str) = item.cast::<PyString>()
                        && let Ok(tag_slice) = py_str.to_str()
                        && !merged_tags.iter().any(|t| t == tag_slice)
                    {
                        merged_tags.push(tag_slice.to_string());
                    }
                });
            }
        }

        let deprecated = deprecated.or(self.deprecated);
        let path_for_closure = path.clone();
        let routes = Arc::clone(&self.route_entries);
        let response_model_capture = response_model.clone();
        let response_class_capture = response_class.clone();

        let decorator = move |args: &Bound<'_, PyTuple>,
                              _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> {
            let py = args.py();
            let func: Py<PyAny> = args.get_item(0)?.unbind();

            let metadata =
                crate::ffi::pydantic::parse_route_metadata(py, func.bind(py), &path_for_closure);

            let final_response_type = if let Some(cls) = &response_class_capture {
                crate::ffi::pydantic::get_response_type_from_class(py, cls.bind(py))
            } else {
                metadata.response_type
            };

            let needs_kwargs = !metadata.body_param_names.is_empty()
                || !metadata.param_validators.is_empty()
                || !metadata.dependencies.is_empty()
                || !metadata.parsed_params.is_empty();

            let mut body_param_name_set: AHashSet<String> = AHashSet::with_capacity(
                metadata.body_param_names.len() + metadata.param_validators.len(),
            );
            metadata
                .parsed_params
                .iter()
                .filter(|p| matches!(p.source, ParameterSource::Body))
                .for_each(|p| {
                    body_param_name_set.insert(p.name.clone());
                });
            metadata.param_validators.iter().for_each(|validator| {
                body_param_name_set.insert(validator.name.clone());
            });

            let body_param_indices = metadata
                .parsed_params
                .iter()
                .enumerate()
                .filter_map(|(idx, param)| {
                    (matches!(param.source, ParameterSource::Body)
                        || body_param_name_set.contains(param.name.as_str()))
                    .then_some(idx)
                })
                .collect();

            let mut handler = crate::routing::types::RouteHandler {
                func: func.clone_ref(py),
                is_async: metadata.is_async,
                is_fast_path: metadata.is_fast_path,
                dependency_needs_request: metadata.dependency_needs_request,
                all_deps_sync: metadata.all_deps_sync,
                needs_kwargs,
                param_validators: metadata.param_validators,
                response_type: final_response_type,
                serialization_hint: if response_model_capture.is_some() {
                    SerializationHint::PydanticModel
                } else {
                    metadata.serialization_hint
                },
                body_param_names: metadata.body_param_names,
                body_param_name_set,
                body_param_indices,
                dependencies: metadata.dependencies,
                parsed_params: metadata.parsed_params,
                default_status,
                response_model: response_model_capture.clone(),
                response_class: response_class_capture.clone(),
                execution_mode: crate::ffi::py_handlers::ExecutionMode::SyncNoArgs,
                cache_response,
                rate_limit_per_second,
            };
            crate::ffi::py_handlers::assign_execution_mode(&mut handler);
            let handler = Arc::new(handler);

            let entry = RouteEntry {
                method,
                path: path_for_closure.clone(),
                handler,
                tags: merged_tags.clone(),
                summary: summary.clone(),
                description: description.clone(),
                deprecated,
                include_in_schema,
            };

            routes.lock().unwrap().push(entry);

            Ok(func)
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
            websockets.lock().unwrap().push(entry);
            Ok(func)
        };
        PyCFunction::new_closure(py, None, None, closure).map(|f| f.into())
    }
}
