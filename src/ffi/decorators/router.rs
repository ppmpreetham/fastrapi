//! The `APIRouter` Python class and its methods.
//! A router holds HTTP routes, WebSocket routes and included sub-routers.

use super::init::set_if_some;
use fastrapi_core::method::HttpMethod;
use fastrapi_routing::route::{RouteEntry, WebSocketEntry};
use pyo3::exceptions::{PyRuntimeError, PyTypeError};
use pyo3::types::{PyDict, PyType};

use parking_lot::Mutex;
use pyo3::Bound;
use pyo3::prelude::{Py, PyAny, PyAnyMethods, PyResult, Python, pyclass, pymethods};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// `APIRouter` class, used to group *path operations*, for example to structure
/// an app in multiple files. It would then be included in the `FastAPI` app, or
/// in another `APIRouter` (ultimately included in the app).
///
/// Read more about it in the
/// [FastAPI docs for Bigger Applications - Multiple Files](https://fastapi.tiangolo.com/tutorial/bigger-applications/).
///
/// ## Example
///
/// ```python
/// from fastrapi import APIRouter, FastAPI
///
/// app = FastAPI()
/// router = APIRouter()
///
///
/// @router.get("/users/", tags=["users"])
/// async def read_users():
///     return [{"username": "Rick"}, {"username": "Morty"}]
///
///
/// app.include_router(router)
/// ```
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
    #[pyo3(get)]
    pub route_class: Option<Py<PyAny>>,

    pub route_entries: Arc<Mutex<Vec<RouteEntry>>>,
    pub websocket_entries: Arc<Mutex<Vec<WebSocketEntry>>>,
    pub sub_routers: Arc<Mutex<Vec<SubRouterMount>>>,
    pub frozen: Arc<AtomicBool>,
    pub cached_flat: Arc<Mutex<Option<Arc<(Vec<RouteEntry>, Vec<WebSocketEntry>)>>>>,
}

impl Default for PyAPIRouter {
    fn default() -> Self {
        Self {
            prefix: String::new(),
            tags: Vec::new(),
            responses: None,
            dependencies: None,
            deprecated: None,
            include_in_schema: true,
            default_response_class: None,
            generate_unique_id_function: None,
            route_class: None,

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
    #[pyo3(signature = (*, prefix=String::new(), tags=None, dependencies=None, responses=None, deprecated=None, include_in_schema=true, default_response_class=None, generate_unique_id_function=None, route_class=None))]
    fn new(
        prefix: String,
        tags: Option<&Bound<'_, PyAny>>,
        dependencies: Option<Py<PyAny>>,
        responses: Option<Py<PyAny>>,
        deprecated: Option<bool>,
        include_in_schema: bool,
        default_response_class: Option<Py<PyAny>>,
        generate_unique_id_function: Option<Py<PyAny>>,
        route_class: Option<Py<PyAny>>,
    ) -> PyResult<Self> {
        let tag_vec = tags
            .and_then(|obj| obj.try_iter().ok())
            .map(|iter| {
                iter.filter_map(|item| item.ok()?.extract::<String>().ok())
                    .collect()
            })
            .unwrap_or_default();

        if let Some(rc) = &route_class {
            let is_type = Python::attach(|py| rc.bind(py).is_instance_of::<PyType>());
            if !is_type {
                return Err(PyTypeError::new_err(
                    "route_class must be a class (subclass of APIRoute)",
                ));
            }
        }

        Ok(Self {
            prefix,
            tags: tag_vec,
            dependencies,
            responses,
            deprecated,
            include_in_schema,
            default_response_class,
            generate_unique_id_function,
            route_class,
            ..Self::default()
        })
    }

    #[pyo3(signature = (path))]
    fn const_get(&self, py: Python<'_>, path: String) -> PyResult<Py<PyAny>> {
        self.create_method_decorator_kw(py, HttpMethod::GET, path, None)
    }

    #[pyo3(signature = (path, endpoint, *, response_model=None, status_code=None, tags=None,
        dependencies=None, summary=None, description=None, response_description=None,
        responses=None, deprecated=None, methods=None, operation_id=None,
        include_in_schema=true, response_class=None, name=None, callbacks=None,
        openapi_extra=None, generate_unique_id_function=None))]
    #[allow(clippy::too_many_arguments)]
    fn add_api_route(
        &self,
        py: Python<'_>,
        path: &str,
        endpoint: &Bound<'_, PyAny>,
        response_model: Option<Py<PyAny>>,
        status_code: Option<u16>,
        tags: Option<Py<PyAny>>,
        dependencies: Option<Py<PyAny>>,
        summary: Option<String>,
        description: Option<String>,
        response_description: Option<String>,
        responses: Option<Py<PyAny>>,
        deprecated: Option<bool>,
        methods: Option<Vec<String>>,
        operation_id: Option<String>,
        include_in_schema: bool,
        response_class: Option<Py<PyAny>>,
        name: Option<String>,
        callbacks: Option<Py<PyAny>>,
        openapi_extra: Option<Py<PyAny>>,
        generate_unique_id_function: Option<Py<PyAny>>,
    ) -> PyResult<()> {
        let kwargs = PyDict::new(py);
        set_if_some(&kwargs, "response_model", response_model)?;
        set_if_some(&kwargs, "status_code", status_code)?;
        set_if_some(&kwargs, "tags", tags)?;
        set_if_some(&kwargs, "dependencies", dependencies)?;
        set_if_some(&kwargs, "summary", summary)?;
        set_if_some(&kwargs, "description", description)?;
        set_if_some(&kwargs, "response_description", response_description)?;
        set_if_some(&kwargs, "responses", responses)?;
        set_if_some(&kwargs, "deprecated", deprecated)?;
        set_if_some(&kwargs, "operation_id", operation_id)?;
        kwargs.set_item("include_in_schema", include_in_schema)?;
        set_if_some(&kwargs, "response_class", response_class)?;
        set_if_some(&kwargs, "name", name)?;
        set_if_some(&kwargs, "callbacks", callbacks)?;
        set_if_some(&kwargs, "openapi_extra", openapi_extra)?;
        set_if_some(
            &kwargs,
            "generate_unique_id_function",
            generate_unique_id_function,
        )?;
        self.add_api_route_impl(py, path, endpoint, methods, &kwargs)
    }

    /// Decorate a WebSocket function.
    ///
    /// Read more about it in the
    /// [FastAPI docs for WebSockets](https://fastapi.tiangolo.com/advanced/websockets/).
    ///
    /// **Example**
    ///
    /// ## Example
    ///
    /// ```python
    /// from fastrapi import APIRouter, FastAPI, WebSocket
    ///
    /// app = FastAPI()
    /// router = APIRouter()
    ///
    /// @router.websocket("/ws")
    /// async def websocket_endpoint(websocket: WebSocket):
    ///     await websocket.accept()
    ///     while True:
    ///         data = await websocket.receive_text()
    ///         await websocket.send_text(f"Message text was: {data}")
    ///
    /// app.include_router(router)
    /// ```
    #[pyo3(signature = (path))]
    fn websocket(&self, py: Python<'_>, path: String) -> PyResult<Py<PyAny>> {
        self.create_ws_decorator(py, path)
    }

    /// Include another `APIRouter` in the same current `APIRouter`.
    ///
    /// Read more about it in the
    /// [FastAPI docs for Bigger Applications](https://fastapi.tiangolo.com/tutorial/bigger-applications/).
    ///
    /// ## Example
    ///
    /// ```python
    /// from fastrapi import APIRouter, FastAPI
    ///
    /// app = FastAPI()
    /// internal_router = APIRouter()
    /// users_router = APIRouter()
    ///
    /// @users_router.get("/users/")
    /// def read_users():
    ///     return [{"name": "Rick"}, {"name": "Morty"}]
    ///
    /// internal_router.include_router(users_router)
    /// app.include_router(internal_router)
    /// ```
    #[pyo3(signature = (router, *, prefix="", tags=None, dependencies=None, responses=None, deprecated=None, include_in_schema=true, default_response_class=None, generate_unique_id_function=None))]
    pub fn include_router(
        &self,
        router: &Bound<'_, Self>,
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

        self.sub_routers.lock().push(SubRouterMount {
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
        router: &Bound<'_, Self>,
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

pub(crate) fn collect_router_routes(
    py: Python<'_>,
    obj: &Bound<'_, PyAny>,
) -> Option<Vec<RouteEntry>> {
    let router = obj.cast::<PyAPIRouter>().ok()?;
    Some(router.borrow().flatten(py).0.clone())
}
