use crate::routing::types::{
    FlatRoute, FlatWebSocket, HttpMethod, ParameterSource, RouteEntry, SerializationHint,
    SubRouterMount, WebSocketEntry,
};
use ahash::AHashSet;
use hyper::StatusCode;
use pyo3::prelude::{Bound, Py, PyAny, PyAnyMethods, PyResult, Python, pyclass, pymethods};
use pyo3::types::{PyCFunction, PyDict, PyTuple};
use pyo3::types::{PyString, PyStringMethods};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[pyclass(name = "APIRouter", skip_from_py_object)]
#[derive(Clone)]
pub struct PyAPIRouter {
    #[pyo3(get)]
    pub prefix: String,
    #[pyo3(get)]
    pub tags: Vec<String>,
    #[pyo3(get)]
    pub dependencies: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub responses: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub deprecated: Option<bool>,
    #[pyo3(get)]
    pub include_in_schema: bool,
    #[pyo3(get)]
    pub default_response_class: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub generate_unique_id_function: Option<Py<PyAny>>,

    // Internal mutable storage
    pub route_entries: Arc<Mutex<Vec<RouteEntry>>>,
    pub websocket_entries: Arc<Mutex<Vec<WebSocketEntry>>>,
    pub sub_routers: Arc<Mutex<Vec<SubRouterMount>>>,
    pub frozen: Arc<AtomicBool>,
    pub cached_flat: Arc<Mutex<Option<Arc<(Vec<FlatRoute>, Vec<FlatWebSocket>)>>>>,
}

impl PyAPIRouter {
    pub fn new_() -> Self {
        Self {
            prefix: String::new(),
            tags: Vec::new(),
            responses: None,
            dependencies: None,
            deprecated: None,
            include_in_schema: true,
            default_response_class: None,
            generate_unique_id_function: None,

            route_entries: Arc::new(Mutex::new(Vec::new())),
            websocket_entries: Arc::new(Mutex::new(Vec::new())),
            sub_routers: Arc::new(Mutex::new(Vec::new())),
            frozen: Arc::new(AtomicBool::new(false)),
            cached_flat: Arc::new(Mutex::new(None)),
        }
    }

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
                            && !merged_tags.iter().any(|t| t == tag_slice) {
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
    fn mark_frozen(&self) {
        self.frozen.store(true, Ordering::Release);
    }
    pub fn freeze(&self, py: Python<'_>) {
        if self.frozen.load(Ordering::Acquire) {
            return;
        }
        let flat = Arc::new(flatten_router(py, self));
        *self.cached_flat.lock().unwrap() = Some(flat);
        self.mark_frozen();
    }

    pub fn flatten(&self, py: Python<'_>) -> Arc<(Vec<FlatRoute>, Vec<FlatWebSocket>)> {
        if self.frozen.load(Ordering::Acquire) {
            if let Some(cached) = self.cached_flat.lock().unwrap().as_ref() {
                return cached.clone();
            }
            return Arc::new(flatten_router(py, self));
        }

        Arc::new(flatten_router(py, self))
    }
}

#[pymethods]
impl PyAPIRouter {
    #[new]
    #[pyo3(signature = (*, prefix="".to_string(), tags=None, dependencies=None, responses=None, deprecated=None, include_in_schema=true, default_response_class=None, generate_unique_id_function=None))]
    fn new(
        prefix: String,
        tags: Option<Py<PyAny>>,
        dependencies: Option<Py<PyAny>>,
        responses: Option<Py<PyAny>>,
        deprecated: Option<bool>,
        include_in_schema: bool,
        default_response_class: Option<Py<PyAny>>,
        generate_unique_id_function: Option<Py<PyAny>>,
    ) -> PyResult<Self> {
        let tag_vec = Python::attach(|py| {
            if let Some(ref tags_obj) = tags {
                let tags_bound = tags_obj.bind(py);
                if let Ok(iter) = tags_bound.try_iter() {
                    return iter
                        .filter_map(|item| item.ok()?.extract::<String>().ok())
                        .collect();
                }
            }
            Vec::new()
        });

        Ok(Self {
            prefix,
            tags: tag_vec,
            dependencies,
            responses,
            deprecated,
            include_in_schema,
            default_response_class,
            generate_unique_id_function,
            route_entries: Arc::new(Mutex::new(Vec::new())),
            websocket_entries: Arc::new(Mutex::new(Vec::new())),
            sub_routers: Arc::new(Mutex::new(Vec::new())),
            frozen: Arc::new(AtomicBool::new(false)),
            cached_flat: Arc::new(Mutex::new(None)),
        })
    }

    #[pyo3(signature = (path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=true, response_model_exclude_unset=false, response_model_exclude_defaults=false, response_model_exclude_none=false, include_in_schema=true, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=false))]
    #[allow(unused_variables)]
    fn get(
        &self,
        py: Python<'_>,
        path: String,
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
        response_model_include: Option<Py<PyAny>>,
        response_model_exclude: Option<Py<PyAny>>,
        response_model_by_alias: bool,
        response_model_exclude_unset: bool,
        response_model_exclude_defaults: bool,
        response_model_exclude_none: bool,
        include_in_schema: bool,
        response_class: Option<Py<PyAny>>,
        name: Option<String>,
        callbacks: Option<Py<PyAny>>,
        openapi_extra: Option<Py<PyAny>>,
        generate_unique_id_function: Option<Py<PyAny>>,
        cache_resp: bool,
    ) -> PyResult<Py<PyAny>> {
        self.create_method_decorator(
            py,
            HttpMethod::GET,
            path,
            status_code,
            response_model.clone(),
            response_class.clone(),
            tags,
            summary,
            description,
            deprecated,
            include_in_schema,
            cache_resp,
        )
    }

    #[pyo3(signature = (path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=true, response_model_exclude_unset=false, response_model_exclude_defaults=false, response_model_exclude_none=false, include_in_schema=true, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=false))]
    #[allow(unused_variables)]
    fn post(
        &self,
        py: Python<'_>,
        path: String,
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
        response_model_include: Option<Py<PyAny>>,
        response_model_exclude: Option<Py<PyAny>>,
        response_model_by_alias: bool,
        response_model_exclude_unset: bool,
        response_model_exclude_defaults: bool,
        response_model_exclude_none: bool,
        include_in_schema: bool,
        response_class: Option<Py<PyAny>>,
        name: Option<String>,
        callbacks: Option<Py<PyAny>>,
        openapi_extra: Option<Py<PyAny>>,
        generate_unique_id_function: Option<Py<PyAny>>,
        cache_resp: bool,
    ) -> PyResult<Py<PyAny>> {
        self.create_method_decorator(
            py,
            HttpMethod::POST,
            path,
            status_code,
            response_model.clone(),
            response_class.clone(),
            tags,
            summary,
            description,
            deprecated,
            include_in_schema,
            cache_resp,
        )
    }

    #[pyo3(signature = (path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=true, response_model_exclude_unset=false, response_model_exclude_defaults=false, response_model_exclude_none=false, include_in_schema=true, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=false))]
    #[allow(unused_variables)]
    fn put(
        &self,
        py: Python<'_>,
        path: String,
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
        response_model_include: Option<Py<PyAny>>,
        response_model_exclude: Option<Py<PyAny>>,
        response_model_by_alias: bool,
        response_model_exclude_unset: bool,
        response_model_exclude_defaults: bool,
        response_model_exclude_none: bool,
        include_in_schema: bool,
        response_class: Option<Py<PyAny>>,
        name: Option<String>,
        callbacks: Option<Py<PyAny>>,
        openapi_extra: Option<Py<PyAny>>,
        generate_unique_id_function: Option<Py<PyAny>>,
        cache_resp: bool,
    ) -> PyResult<Py<PyAny>> {
        self.create_method_decorator(
            py,
            HttpMethod::PUT,
            path,
            status_code,
            response_model.clone(),
            response_class.clone(),
            tags,
            summary,
            description,
            deprecated,
            include_in_schema,
            cache_resp,
        )
    }

    #[pyo3(signature = (path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=true, response_model_exclude_unset=false, response_model_exclude_defaults=false, response_model_exclude_none=false, include_in_schema=true, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=false))]
    #[allow(unused_variables)]
    fn delete(
        &self,
        py: Python<'_>,
        path: String,
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
        response_model_include: Option<Py<PyAny>>,
        response_model_exclude: Option<Py<PyAny>>,
        response_model_by_alias: bool,
        response_model_exclude_unset: bool,
        response_model_exclude_defaults: bool,
        response_model_exclude_none: bool,
        include_in_schema: bool,
        response_class: Option<Py<PyAny>>,
        name: Option<String>,
        callbacks: Option<Py<PyAny>>,
        openapi_extra: Option<Py<PyAny>>,
        generate_unique_id_function: Option<Py<PyAny>>,
        cache_resp: bool,
    ) -> PyResult<Py<PyAny>> {
        self.create_method_decorator(
            py,
            HttpMethod::DELETE,
            path,
            status_code,
            response_model.clone(),
            response_class.clone(),
            tags,
            summary,
            description,
            deprecated,
            include_in_schema,
            cache_resp,
        )
    }

    #[pyo3(signature = (path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=true, response_model_exclude_unset=false, response_model_exclude_defaults=false, response_model_exclude_none=false, include_in_schema=true, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=false))]
    #[allow(unused_variables)]
    fn patch(
        &self,
        py: Python<'_>,
        path: String,
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
        response_model_include: Option<Py<PyAny>>,
        response_model_exclude: Option<Py<PyAny>>,
        response_model_by_alias: bool,
        response_model_exclude_unset: bool,
        response_model_exclude_defaults: bool,
        response_model_exclude_none: bool,
        include_in_schema: bool,
        response_class: Option<Py<PyAny>>,
        name: Option<String>,
        callbacks: Option<Py<PyAny>>,
        openapi_extra: Option<Py<PyAny>>,
        generate_unique_id_function: Option<Py<PyAny>>,
        cache_resp: bool,
    ) -> PyResult<Py<PyAny>> {
        self.create_method_decorator(
            py,
            HttpMethod::PATCH,
            path,
            status_code,
            response_model.clone(),
            response_class.clone(),
            tags,
            summary,
            description,
            deprecated,
            include_in_schema,
            cache_resp,
        )
    }

    #[pyo3(signature = (path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=true, response_model_exclude_unset=false, response_model_exclude_defaults=false, response_model_exclude_none=false, include_in_schema=true, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=false))]
    #[allow(unused_variables)]
    fn options(
        &self,
        py: Python<'_>,
        path: String,
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
        response_model_include: Option<Py<PyAny>>,
        response_model_exclude: Option<Py<PyAny>>,
        response_model_by_alias: bool,
        response_model_exclude_unset: bool,
        response_model_exclude_defaults: bool,
        response_model_exclude_none: bool,
        include_in_schema: bool,
        response_class: Option<Py<PyAny>>,
        name: Option<String>,
        callbacks: Option<Py<PyAny>>,
        openapi_extra: Option<Py<PyAny>>,
        generate_unique_id_function: Option<Py<PyAny>>,
        cache_resp: bool,
    ) -> PyResult<Py<PyAny>> {
        self.create_method_decorator(
            py,
            HttpMethod::OPTIONS,
            path,
            status_code,
            response_model.clone(),
            response_class.clone(),
            tags,
            summary,
            description,
            deprecated,
            include_in_schema,
            cache_resp,
        )
    }

    #[pyo3(signature = (path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=true, response_model_exclude_unset=false, response_model_exclude_defaults=false, response_model_exclude_none=false, include_in_schema=true, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=false))]
    #[allow(unused_variables)]
    fn head(
        &self,
        py: Python<'_>,
        path: String,
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
        response_model_include: Option<Py<PyAny>>,
        response_model_exclude: Option<Py<PyAny>>,
        response_model_by_alias: bool,
        response_model_exclude_unset: bool,
        response_model_exclude_defaults: bool,
        response_model_exclude_none: bool,
        include_in_schema: bool,
        response_class: Option<Py<PyAny>>,
        name: Option<String>,
        callbacks: Option<Py<PyAny>>,
        openapi_extra: Option<Py<PyAny>>,
        generate_unique_id_function: Option<Py<PyAny>>,
        cache_resp: bool,
    ) -> PyResult<Py<PyAny>> {
        self.create_method_decorator(
            py,
            HttpMethod::HEAD,
            path,
            status_code,
            response_model.clone(),
            response_class.clone(),
            tags,
            summary,
            description,
            deprecated,
            include_in_schema,
            cache_resp,
        )
    }

    #[pyo3(signature = (path))]
    fn const_get(&self, py: Python<'_>, path: String) -> PyResult<Py<PyAny>> {
        self.create_method_decorator(
            py,
            HttpMethod::GET,
            path,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            true,
            true,
        )
    }

    #[pyo3(signature = (path))]
    fn websocket(&self, py: Python<'_>, path: String) -> PyResult<Py<PyAny>> {
        self.create_ws_decorator(py, path)
    }

    #[pyo3(signature = (router, *, prefix="".to_string(), tags=None))]
    pub fn include_router(
        &self,
        py: Python<'_>,
        router: Py<PyAPIRouter>,
        prefix: String,
        tags: Option<Py<PyAny>>,
    ) -> PyResult<()> {
        if self.frozen.load(Ordering::Relaxed) {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "Cannot modify router after it has been frozen",
            ));
        }
        let tag_vec: Vec<String> = if let Some(ref tags_obj) = tags {
            let tags_bound = tags_obj.bind(py);
            if let Ok(iter) = tags_bound.try_iter() {
                iter.filter_map(|item| item.ok()?.extract::<String>().ok())
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        self.sub_routers.lock().unwrap().push(SubRouterMount {
            router,
            prefix,
            tags: tag_vec,
        });

        Ok(())
    }
}

fn flatten_router(py: Python<'_>, root: &PyAPIRouter) -> (Vec<FlatRoute>, Vec<FlatWebSocket>) {
    let mut routes = Vec::new();
    let mut ws_routes = Vec::new();
    let mut stack = vec![(root.clone(), String::new(), Vec::<String>::new())];

    while let Some((router, prefix, parent_tags)) = stack.pop() {
        router.mark_frozen();

        let full_prefix = join_path(&prefix, &router.prefix);

        let mut current_tags = parent_tags;
        for tag in &router.tags {
            if !current_tags.contains(tag) {
                current_tags.push(tag.clone());
            }
        }

        let route_entries = router.route_entries.lock().unwrap().clone();
        routes.extend(route_entries.into_iter().map(|entry| {
            let mut tags = current_tags.clone();
            for tag in &entry.tags {
                if !tags.contains(tag) {
                    tags.push(tag.clone());
                }
            }

            FlatRoute {
                method: entry.method,
                path: join_path(&full_prefix, &entry.path),
                handler: entry.handler,
                tags,
                summary: entry.summary.clone(),
                description: entry.description.clone(),
                deprecated: entry.deprecated,
                include_in_schema: entry.include_in_schema,
            }
        }));

        let ws_entries = router.websocket_entries.lock().unwrap().clone();
        ws_routes.extend(ws_entries.into_iter().map(|ws| FlatWebSocket {
            path: join_path(&full_prefix, &ws.path),
            handler: ws.handler.clone_ref(py),
        }));

        let subs = router.sub_routers.lock().unwrap().clone();
        for sub in subs {
            let sub_router = sub.router.bind(py).borrow();

            let mut sub_tags = current_tags.clone();
            for tag in &sub.tags {
                if !sub_tags.contains(tag) {
                    sub_tags.push(tag.clone());
                }
            }

            stack.push((
                sub_router.clone(),
                join_path(&full_prefix, &sub.prefix),
                sub_tags,
            ));
        }
    }

    (routes, ws_routes)
}

fn join_path(a: &str, b: &str) -> String {
    let a_ends = a.ends_with('/');
    let b_starts = b.starts_with('/');

    let capacity = match (a_ends, b_starts) {
        (true, true) => a.len() + b.len() - 1,
        (false, false) => a.len() + b.len() + 1,
        _ => a.len() + b.len(),
    };

    let mut path = String::with_capacity(capacity);
    path.push_str(a);

    match (a_ends, b_starts) {
        (true, true) => {
            path.pop();
            path.push_str(b);
        }
        (false, false) => {
            path.push('/');
            path.push_str(b);
        }
        _ => {
            path.push_str(b);
        }
    }
    path
}
