mod init;
mod requests;

pub use crate::routing::tree::*;

use crate::routing::types::{HttpMethod, RouteEntry, SubRouterMount, WebSocketEntry};

use crate::utils::LockExt;
use pyo3::Bound;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::{Py, PyAny, PyAnyMethods, PyResult, Python, pyclass, pymethods};
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

    pub route_entries: Arc<Mutex<Vec<RouteEntry>>>,
    pub websocket_entries: Arc<Mutex<Vec<WebSocketEntry>>>,
    pub sub_routers: Arc<Mutex<Vec<SubRouterMount>>>,
    pub frozen: Arc<AtomicBool>,
    pub cached_flat: Arc<Mutex<Option<Arc<(Vec<RouteEntry>, Vec<WebSocketEntry>)>>>>,
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
            tags.as_ref()
                .and_then(|obj| obj.bind(py).try_iter().ok())
                .map(|iter| {
                    iter.filter_map(|item| item.ok()?.extract::<String>().ok())
                        .collect()
                })
                .unwrap_or_default()
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

    #[pyo3(signature = (path))]
    fn const_get(&self, py: Python<'_>, path: String) -> PyResult<Py<PyAny>> {
        self.create_method_decorator_kw(py, HttpMethod::GET, path, None)
    }

    #[pyo3(signature = (path))]
    fn websocket(&self, py: Python<'_>, path: String) -> PyResult<Py<PyAny>> {
        self.create_ws_decorator(py, path)
    }

    #[pyo3(signature = (router, *, prefix="", tags=None, dependencies=None, responses=None, deprecated=None, include_in_schema=true, default_response_class=None, generate_unique_id_function=None))]
    pub fn include_router(
        &self,
        router: &Bound<'_, PyAPIRouter>,
        prefix: &str,
        tags: Option<&Bound<'_, PyAny>>,
        dependencies: Option<&Bound<'_, PyAny>>,
        responses: Option<&Bound<'_, PyAny>>,
        deprecated: Option<bool>,
        include_in_schema: bool,
        default_response_class: Option<&Bound<'_, PyAny>>,
        generate_unique_id_function: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        if self.frozen.load(Ordering::Relaxed) {
            return Err(PyRuntimeError::new_err(
                "Cannot modify router after it has been frozen",
            ));
        }

        let tag_vec: Vec<String> = tags
            .and_then(|t| t.try_iter().ok())
            .into_iter()
            .flatten()
            .filter_map(|item| item.ok()?.extract::<String>().ok())
            .collect();

        self.sub_routers.lock_or_panic().push(SubRouterMount {
            router: router.clone().unbind(),
            prefix: prefix.to_owned(),
            tags: tag_vec,
            dependencies: dependencies.map(|d| d.clone().unbind()),
            responses: responses.map(|r| r.clone().unbind()),
            deprecated,
            include_in_schema,
            default_response_class: default_response_class.map(|d| d.clone().unbind()),
            generate_unique_id_function: generate_unique_id_function.map(|g| g.clone().unbind()),
        });

        Ok(())
    }

    #[pyo3(signature = (prefix, router, *, tags=None, dependencies=None, responses=None, deprecated=None, include_in_schema=true, default_response_class=None, generate_unique_id_function=None))]
    fn nest(
        &self,
        prefix: &str,
        router: &Bound<'_, PyAPIRouter>,
        tags: Option<&Bound<'_, PyAny>>,
        dependencies: Option<&Bound<'_, PyAny>>,
        responses: Option<&Bound<'_, PyAny>>,
        deprecated: Option<bool>,
        include_in_schema: bool,
        default_response_class: Option<&Bound<'_, PyAny>>,
        generate_unique_id_function: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        self.include_router(
            router,
            prefix,
            tags,
            dependencies,
            responses,
            deprecated,
            include_in_schema,
            default_response_class,
            generate_unique_id_function,
        )
    }
}
