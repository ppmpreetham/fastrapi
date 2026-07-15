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
    pub fn create_method_decorator_kw(
        &self,
        py: Python<'_>,
        method: HttpMethod,
        path: String,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let extract_opt = |key: &str| -> Option<Py<PyAny>> {
            kwargs
                .and_then(|kw| kw.get_item(key).ok())
                .map(|x| x.unbind())
        };

        let status_code: Option<u16> = kwargs
            .and_then(|kw| kw.get_item("status_code").ok())
            .and_then(|x| x.extract().ok());

        let mut bypass_serialization = false;
        let response_model = if let Some(kw) = kwargs {
            if let Ok(rm) = kw.get_item("response_model") {
                if rm.is_none() {
                    bypass_serialization = true;
                }
                Some(rm.unbind())
            } else {
                None
            }
        } else {
            None
        };

        let response_class = extract_opt("response_class");
        let tags = extract_opt("tags");
        let summary: Option<String> = kwargs
            .and_then(|kw| kw.get_item("summary").ok())
            .and_then(|x| x.extract().ok());
        let description: Option<String> = kwargs
            .and_then(|kw| kw.get_item("description").ok())
            .and_then(|x| x.extract().ok());
        let deprecated: Option<bool> = kwargs
            .and_then(|kw| kw.get_item("deprecated").ok())
            .and_then(|x| x.extract().ok());
        let include_in_schema: bool = kwargs
            .and_then(|kw| kw.get_item("include_in_schema").ok())
            .and_then(|x| x.extract().ok())
            .unwrap_or(true);
        let cache_response: bool = kwargs
            .and_then(|kw| kw.get_item("cache_resp").ok())
            .and_then(|x| x.extract().ok())
            .unwrap_or(false);
        let rate_limit_per_second: Option<u32> = kwargs
            .and_then(|kw| kw.get_item("rate_limit").ok())
            .and_then(|x| x.extract().ok());

        let response_description: Option<String> = kwargs
            .and_then(|kw| kw.get_item("response_description").ok())
            .and_then(|x| x.extract().ok());

        let operation_id: Option<String> = kwargs
            .and_then(|kw| kw.get_item("operation_id").ok())
            .and_then(|x| x.extract().ok());

        let responses = kwargs
            .and_then(|kw| kw.get_item("responses").ok())
            .map(|d| crate::utils::py_any_to_json(py, &d));

        let openapi_extra = kwargs
            .and_then(|kw| kw.get_item("openapi_extra").ok())
            .map(|d| crate::utils::py_any_to_json(py, &d));

        let callbacks = kwargs
            .and_then(|kw| kw.get_item("callbacks").ok())
            .and_then(|x| crate::utils::openapi::parse_callbacks_to_json(py, &x));

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
                serialization_hint: if bypass_serialization {
                    SerializationHint::Unknown
                } else if response_model_capture.is_some() {
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
                responses: responses.clone(),
                callbacks: None,
                openapi_extra: openapi_extra.clone(),
                response_description: response_description.clone(),
                operation_id: operation_id.clone(),
                bypass_serialization,
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
                response_description: response_description.clone(),
                operation_id: operation_id.clone(),
                responses: responses.clone(),
                openapi_extra: openapi_extra.clone(),
                callbacks: callbacks.clone(),
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
