//! The `APIRouter` Python class and its methods.
//! A router holds HTTP routes, WebSocket routes and included sub-routers.

use fastrapi_core::method::HttpMethod;
use fastrapi_http::exceptions::PyFastAPIError;
use fastrapi_pyutil::subclass::NativeSubclass;
use fastrapi_routing::route::{RouteEntry, WebSocketEntry};
use pyo3::exceptions::{PyAssertionError, PyDeprecationWarning, PyTypeError};
use pyo3::types::{PyDict, PyList, PyType};

use crate::engine::app::{CheckDir, frontend_mount};
use crate::engine::config::FrontendMount;
use parking_lot::Mutex;
use pyo3::Bound;
use pyo3::prelude::{Py, PyAny, PyAnyMethods, PyErr, PyResult, Python, pyclass, pymethods};
use pyo3::sync::PyOnceLock;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};

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
#[pyclass(name = "APIRouter", subclass, skip_from_py_object)]
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
    pub default_response_class: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub generate_unique_id_function: Option<Py<PyAny>>,
    pub route_class: Option<Py<PyAny>>,

    pub route_entries: Arc<Mutex<Vec<RouteEntry>>>,
    pub websocket_entries: Arc<Mutex<Vec<WebSocketEntry>>>,
    pub sub_routers: Arc<Mutex<Vec<SubRouterMount>>>,
    pub frozen: Arc<AtomicBool>,
    pub revision: Arc<AtomicU64>,
    pub cached_flat: Arc<Mutex<Option<Arc<(Vec<RouteEntry>, Vec<WebSocketEntry>)>>>>,
    pub startup_handlers: Arc<Mutex<Vec<Py<PyAny>>>>,
    pub shutdown_handlers: Arc<Mutex<Vec<Py<PyAny>>>>,
    pub lifespan: Arc<Mutex<Option<Py<PyAny>>>>,
    #[pyo3(get)]
    pub callbacks: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub redirect_slashes: bool,
    #[pyo3(get)]
    pub default: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub dependency_overrides_provider: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub strict_content_type: Option<bool>,
    pub mounts: Arc<Mutex<Vec<(String, Py<PyAny>, Option<String>)>>>,
    pub hosts: Arc<Mutex<Vec<(String, Py<PyAny>, Option<String>)>>>,
    pub frontends: Arc<Mutex<Vec<FrontendMount>>>,
    /// FastAPI's routes that are tried after all the others.
    #[pyo3(get, name = "_low_priority_routes")]
    pub low_priority_routes: Py<PyList>,
    pub asgi: Arc<PyOnceLock<Py<PyAny>>>,
}

fastrapi_pyutil::cached_py_import!(STARLETTE_ROUTER, "starlette.routing", "Router");
fastrapi_pyutil::cached_py_import!(STARLETTE_MATCH, "starlette.routing", "Match");

impl PyAPIRouter {
    fn asgi<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        slf.borrow()
            .asgi
            .get_or_try_init(py, || {
                let kwargs = PyDict::new(py);
                for name in ["openapi_url", "docs_url", "redoc_url"] {
                    kwargs.set_item(name, py.None())?;
                }
                let app = py
                    .get_type::<crate::engine::config::FastrAPI>()
                    .call((), Some(&kwargs))?;
                app.call_method1("include_router", (slf,))?;
                Ok::<_, PyErr>(app.unbind())
            })
            .map(|app| app.bind(py).clone())
    }

    #[must_use]
    pub fn with_callbacks(self, callbacks: Option<Py<PyAny>>) -> Self {
        match callbacks {
            Some(callbacks) => Self {
                callbacks: Some(callbacks),
                ..self
            },
            None => self,
        }
    }
}

static PUBLIC_TYPE: NativeSubclass =
    NativeSubclass::new("starlette.routing", "Router", "fastrapi.routing");

fn ensure_valid_prefix(prefix: &str) -> PyResult<()> {
    if !prefix.is_empty() && !prefix.starts_with('/') {
        return Err(PyAssertionError::new_err(
            "A path prefix must start with '/'",
        ));
    }
    if prefix.ends_with('/') {
        return Err(PyAssertionError::new_err(
            "A path prefix must not end with '/', as the routes will start with '/'",
        ));
    }
    Ok(())
}

/// The `APIRouter` class users see, which is also a Starlette `Router`.
type EventHandlers = Arc<Mutex<Vec<Py<PyAny>>>>;

/// FastAPI asserts that the event type is `startup` or `shutdown`.
fn register_event_handler(
    startup: &EventHandlers,
    shutdown: &EventHandlers,
    event_type: &str,
    func: Py<PyAny>,
) -> PyResult<()> {
    match event_type {
        "startup" => startup.lock().push(func),
        "shutdown" => shutdown.lock().push(func),
        _ => return Err(PyAssertionError::new_err(())),
    }
    Ok(())
}

pub(crate) fn warn_on_event_deprecated(py: Python<'_>) -> PyResult<()> {
    PyErr::warn(
        py,
        &py.get_type::<PyDeprecationWarning>(),
        c"
        on_event is deprecated, use lifespan event handlers instead.

        Read more about it in the
        [FastAPI docs for Lifespan Events](https://fastapi.tiangolo.com/advanced/events/).
        ",
        1,
    )
}

pub(crate) fn api_router_type(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    PUBLIC_TYPE.get::<PyAPIRouter>(py)
}

/// Creates a public `APIRouter` with the constructor's keyword arguments.
pub(crate) fn new_api_router<'py>(
    py: Python<'py>,
    kwargs: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyAPIRouter>> {
    Ok(api_router_type(py)?
        .call((), kwargs)?
        .cast_into::<PyAPIRouter>()?)
}

impl Default for PyAPIRouter {
    fn default() -> Self {
        let (dependencies, responses, callbacks, low_priority_routes) = Python::attach(|py| {
            (
                PyList::empty(py).into_any().unbind(),
                PyDict::new(py).into_any().unbind(),
                PyList::empty(py).into_any().unbind(),
                PyList::empty(py).unbind(),
            )
        });
        Self {
            prefix: String::new(),
            tags: Vec::new(),
            responses: Some(responses),
            dependencies: Some(dependencies),
            deprecated: None,
            include_in_schema: true,
            default_response_class: None,
            generate_unique_id_function: None,
            route_class: None,

            route_entries: Arc::new(Mutex::new(Vec::new())),
            websocket_entries: Arc::new(Mutex::new(Vec::new())),
            sub_routers: Arc::new(Mutex::new(Vec::new())),
            frozen: Arc::new(AtomicBool::new(false)),
            revision: Arc::new(AtomicU64::new(0)),
            cached_flat: Arc::new(Mutex::new(None)),
            startup_handlers: Arc::new(Mutex::new(Vec::new())),
            shutdown_handlers: Arc::new(Mutex::new(Vec::new())),
            lifespan: Arc::new(Mutex::new(None)),
            callbacks: Some(callbacks),
            redirect_slashes: true,
            default: None,
            dependency_overrides_provider: None,
            strict_content_type: None,
            mounts: Arc::new(Mutex::new(Vec::new())),
            hosts: Arc::new(Mutex::new(Vec::new())),
            frontends: Arc::new(Mutex::new(Vec::new())),
            low_priority_routes,
            asgi: Arc::new(PyOnceLock::new()),
        }
    }
}

#[pymethods]
impl PyAPIRouter {
    fn __call__<'py>(
        slf: &Bound<'py, Self>,
        scope: &Bound<'py, PyAny>,
        receive: &Bound<'py, PyAny>,
        send: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        Self::asgi(slf)?.call1((scope, receive, send))
    }

    fn app<'py>(
        slf: &Bound<'py, Self>,
        scope: &Bound<'py, PyAny>,
        receive: &Bound<'py, PyAny>,
        send: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        Self::__call__(slf, scope, receive, send)
    }

    fn handle<'py>(
        slf: &Bound<'py, Self>,
        scope: &Bound<'py, PyAny>,
        receive: &Bound<'py, PyAny>,
        send: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        Self::__call__(slf, scope, receive, send)
    }

    fn lifespan<'py>(
        slf: &Bound<'py, Self>,
        scope: &Bound<'py, PyAny>,
        receive: &Bound<'py, PyAny>,
        send: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        Self::__call__(slf, scope, receive, send)
    }

    fn not_found<'py>(
        slf: &Bound<'py, Self>,
        scope: &Bound<'py, PyAny>,
        receive: &Bound<'py, PyAny>,
        send: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        STARLETTE_ROUTER
            .get(slf.py())?
            .call_method1("not_found", (slf, scope, receive, send))
    }

    fn matches<'py>(
        &self,
        py: Python<'py>,
        scope: &Bound<'py, PyAny>,
    ) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyDict>)> {
        // The router itself never matches; the parameter is named `scope` as in Starlette.
        let _ = scope;
        Ok((STARLETTE_MATCH.get(py)?.getattr("NONE")?, PyDict::new(py)))
    }

    #[new]
    #[pyo3(signature = (*, prefix=String::new(), tags=None, dependencies=None, responses=None, deprecated=None, include_in_schema=true, default_response_class=None, generate_unique_id_function=None, route_class=None, on_startup=None, on_shutdown=None, lifespan=None, routes=None, redirect_slashes=true, default=None, callbacks=None, dependency_overrides_provider=None, strict_content_type=None))]
    #[expect(
        clippy::too_many_arguments,
        reason = "the signature matches FastAPI's APIRouter"
    )]
    fn new(
        py: Python<'_>,
        prefix: String,
        tags: Option<&Bound<'_, PyAny>>,
        dependencies: Option<Py<PyAny>>,
        responses: Option<Py<PyAny>>,
        deprecated: Option<bool>,
        include_in_schema: bool,
        default_response_class: Option<Py<PyAny>>,
        generate_unique_id_function: Option<Py<PyAny>>,
        route_class: Option<Py<PyAny>>,
        on_startup: Option<Vec<Py<PyAny>>>,
        on_shutdown: Option<Vec<Py<PyAny>>>,
        lifespan: Option<Py<PyAny>>,
        routes: Option<&Bound<'_, PyAny>>,
        redirect_slashes: bool,
        default: Option<Py<PyAny>>,
        callbacks: Option<Py<PyAny>>,
        dependency_overrides_provider: Option<Py<PyAny>>,
        strict_content_type: Option<bool>,
    ) -> PyResult<Self> {
        ensure_valid_prefix(&prefix)?;
        let tag_vec = tags
            .and_then(|obj| obj.try_iter().ok())
            .map(|iter| {
                iter.filter_map(|item| super::tag_name(&item.ok()?))
                    .collect()
            })
            .unwrap_or_default();

        super::tree::validate_dependencies(py, dependencies.as_ref())?;
        let dependencies = match dependencies.filter(|deps| !deps.is_none(py)) {
            Some(deps) => py.get_type::<PyList>().call1((deps,))?,
            None => PyList::empty(py).into_any(),
        };
        let truthy_or = |value: Option<Py<PyAny>>, empty: Bound<'_, PyAny>| -> Py<PyAny> {
            value
                .filter(|value| value.bind(py).is_truthy().unwrap_or(false))
                .unwrap_or_else(|| empty.unbind())
        };
        let responses = truthy_or(responses, PyDict::new(py).into_any());
        let callbacks = truthy_or(callbacks, PyList::empty(py).into_any());
        if let Some(rc) = &route_class {
            let is_type = Python::attach(|py| rc.bind(py).is_instance_of::<PyType>());
            if !is_type {
                return Err(PyTypeError::new_err(
                    "route_class must be a class (subclass of APIRoute)",
                ));
            }
        }

        let router = Self {
            prefix,
            tags: tag_vec,
            dependencies: Some(dependencies.unbind()),
            responses: Some(responses),
            deprecated,
            include_in_schema,
            default_response_class,
            generate_unique_id_function,
            route_class,
            startup_handlers: Arc::new(Mutex::new(on_startup.unwrap_or_default())),
            shutdown_handlers: Arc::new(Mutex::new(on_shutdown.unwrap_or_default())),
            lifespan: Arc::new(Mutex::new(lifespan)),
            callbacks: Some(callbacks),
            redirect_slashes,
            default,
            dependency_overrides_provider,
            strict_content_type,
            ..Self::default()
        };
        if let Some(routes) = routes.filter(|routes| !routes.is_none()) {
            crate::ffi::openapi::add_route_objects(py, &router, routes)?;
        }
        Ok(router)
    }

    #[pyo3(signature = (path, app, name=None))]
    fn mount(&self, path: String, app: Py<PyAny>, name: Option<String>) -> PyResult<()> {
        super::ensure_routed_path(&path)?;
        self.ensure_mutable()?;
        self.mounts.lock().push((path, app, name));
        self.touch();
        Ok(())
    }

    #[pyo3(
        signature = (path, *, directory, fallback=Some("auto".to_string()), check_dir=CheckDir::Mode("auto".to_owned())),
        text_signature = "(self, path, *, directory, fallback='auto', check_dir='auto')"
    )]
    fn frontend(
        &self,
        py: Python<'_>,
        path: String,
        directory: PathBuf,
        fallback: Option<String>,
        check_dir: CheckDir,
    ) -> PyResult<()> {
        self.ensure_mutable()?;
        let mount = frontend_mount(py, path, directory, fallback, check_dir)?;
        self.frontends.lock().push(mount);
        self.touch();
        Ok(())
    }

    #[pyo3(signature = (host, app, name=None))]
    fn host(&self, host: String, app: Py<PyAny>, name: Option<String>) -> PyResult<()> {
        if host.starts_with('/') {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "Host must not start with '/'",
            ));
        }
        self.ensure_mutable()?;
        self.hosts.lock().push((host, app, name));
        self.touch();
        Ok(())
    }

    fn _mark_routes_changed(&self) {
        self.touch();
    }

    #[getter]
    fn default_response_class(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        match &self.default_response_class {
            Some(class) => Ok(class.clone_ref(py)),
            None => Py::new(
                py,
                crate::ffi::compat::DefaultPlaceholder::new(
                    py.get_type::<fastrapi_http::responses::PyJSONResponse>()
                        .into_any()
                        .unbind(),
                ),
            )
            .map(Py::into_any),
        }
    }

    #[getter]
    fn route_class(&self, py: Python<'_>) -> Py<PyAny> {
        self.route_class.as_ref().map_or_else(
            || {
                py.get_type::<crate::ffi::routing::PyAPIRoute>()
                    .into_any()
                    .unbind()
            },
            |class| class.clone_ref(py),
        )
    }

    #[setter]
    fn set_route_class(&mut self, route_class: Py<PyAny>) {
        self.route_class = Some(route_class);
    }

    #[getter]
    fn on_startup<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        PyList::new(
            py,
            self.startup_handlers.lock().iter().map(|h| h.clone_ref(py)),
        )
    }

    #[getter]
    fn on_shutdown<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        PyList::new(
            py,
            self.shutdown_handlers
                .lock()
                .iter()
                .map(|h| h.clone_ref(py)),
        )
    }

    #[getter]
    fn lifespan_context(slf: &Bound<'_, Self>, py: Python<'_>) -> PyResult<Py<PyAny>> {
        if let Some(lifespan) = slf.borrow().lifespan.lock().as_ref() {
            return Ok(lifespan.clone_ref(py));
        }
        Ok(crate::ffi::compat::py_helpers(py)?
            .getattr("default_lifespan")?
            .call1((slf,))?
            .unbind())
    }

    #[setter]
    fn set_lifespan_context(&self, lifespan: Option<Py<PyAny>>) {
        *self.lifespan.lock() = lifespan;
    }

    fn add_event_handler(&self, event_type: &str, func: Py<PyAny>) -> PyResult<()> {
        register_event_handler(
            &self.startup_handlers,
            &self.shutdown_handlers,
            event_type,
            func,
        )
    }

    fn on_event(&self, py: Python<'_>, event_type: String) -> PyResult<Py<PyAny>> {
        warn_on_event_deprecated(py)?;
        let startup = Arc::clone(&self.startup_handlers);
        let shutdown = Arc::clone(&self.shutdown_handlers);
        let decorator = move |args: &Bound<'_, pyo3::types::PyTuple>,
                              _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> {
            let func = args.get_item(0)?.unbind();
            register_event_handler(&startup, &shutdown, &event_type, func.clone_ref(args.py()))?;
            Ok(func)
        };
        pyo3::types::PyCFunction::new_closure(py, Some(c"on_event"), None, decorator)
            .map(Into::into)
    }

    #[pyo3(signature = (path))]
    fn const_get(&self, py: Python<'_>, path: String) -> PyResult<Py<PyAny>> {
        self.create_method_decorator_kw(py, HttpMethod::GET, path, None)
    }

    #[pyo3(signature = (path, endpoint, *, methods=None, **kwargs))]
    fn add_api_route(
        &self,
        py: Python<'_>,
        path: &str,
        endpoint: &Bound<'_, PyAny>,
        methods: Option<Vec<String>>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        let kwargs = super::init::add_api_route_kwargs(py, kwargs)?;
        self.add_api_route_impl(py, path, endpoint, methods, &kwargs)
    }

    #[getter]
    fn routes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let (routes, websockets) = self.snapshot(py)?;
        crate::ffi::routing::route_objects(py, &routes, &websockets)
    }

    #[pyo3(signature = (name, /, **path_params))]
    fn url_path_for(
        &self,
        py: Python<'_>,
        name: &str,
        path_params: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        self.url_path_for_name(py, name, path_params)
    }

    #[pyo3(signature = (path, methods=None, **kwargs))]
    fn api_route(
        &self,
        py: Python<'_>,
        path: &str,
        methods: Option<Vec<String>>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        self.create_api_route_decorator(py, path, methods, kwargs)
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
    #[pyo3(signature = (path, name=None, *, dependencies=None))]
    fn websocket(
        &self,
        py: Python<'_>,
        path: String,
        name: Option<String>,
        dependencies: Option<Py<PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        self.create_ws_decorator(py, path, name, dependencies)
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
    #[pyo3(signature = (router, *, prefix="", tags=None, dependencies=None, responses=None, deprecated=None, include_in_schema=true, default_response_class=None, callbacks=None, generate_unique_id_function=None))]
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
        callbacks: Option<&Bound<'_, PyAny>>,
        generate_unique_id_function: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let py = router.py();
        if std::ptr::eq(self, &raw const *router.borrow()) {
            return Err(PyAssertionError::new_err(
                "Cannot include the same APIRouter instance into itself. Did you mean to include a different router?",
            ));
        }
        ensure_valid_prefix(prefix)?;
        if prefix.is_empty()
            && router.borrow().prefix.is_empty()
            && let Some(route) = router
                .borrow()
                .route_entries
                .lock()
                .iter()
                .find(|route| route.path.is_empty())
        {
            return Err(PyErr::from_type(
                py.get_type::<PyFastAPIError>(),
                format!(
                    "Prefix and path cannot be both empty (path operation: {})",
                    route.name
                ),
            ));
        }
        self.ensure_mutable()?;

        let tag_vec: Vec<String> = tags
            .and_then(|t| t.try_iter().ok())
            .into_iter()
            .flatten()
            .filter_map(|item| super::tag_name(&item.ok()?))
            .collect();

        let dependencies_owned = dependencies.map(|d| d.clone().unbind());
        super::tree::validate_dependencies(py, dependencies_owned.as_ref())?;
        let included = router.borrow();
        self.startup_handlers.lock().extend(
            included
                .startup_handlers
                .lock()
                .iter()
                .map(|handler| handler.clone_ref(py)),
        );
        self.shutdown_handlers.lock().extend(
            included
                .shutdown_handlers
                .lock()
                .iter()
                .map(|handler| handler.clone_ref(py)),
        );
        let nested = included
            .lifespan
            .lock()
            .as_ref()
            .map(|lifespan| lifespan.clone_ref(py));
        drop(included);
        if let Some(nested) = nested {
            let mut own = self.lifespan.lock();
            let merged = match own.take() {
                Some(original) => crate::ffi::compat::py_helpers(py)?
                    .getattr("merge_lifespan_context")?
                    .call1((original, nested))?
                    .unbind(),
                None => nested,
            };
            *own = Some(merged);
        }

        self.touch();
        self.sub_routers.lock().push(SubRouterMount {
            router: router.clone().unbind(),
            prefix: prefix.to_owned(),
            tags: tag_vec,
            dependencies: dependencies_owned,
            responses: responses.map(|r| r.clone().unbind()),
            deprecated,
            include_in_schema,
            default_response_class: default_response_class.map(|d| d.clone().unbind()),
            generate_unique_id_function: generate_unique_id_function.map(|g| g.clone().unbind()),
            callbacks: callbacks
                .filter(|callbacks| !callbacks.is_none())
                .map(|callbacks| callbacks.clone().unbind()),
        });

        Ok(())
    }

    #[pyo3(signature = (prefix, router, *, tags=None, dependencies=None, responses=None, deprecated=None, include_in_schema=true, default_response_class=None, callbacks=None, generate_unique_id_function=None))]
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
        callbacks: Option<&Bound<'_, PyAny>>,
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
            callbacks,
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
    pub callbacks: Option<Py<PyAny>>,
}

pub(crate) fn collect_router_routes(
    py: Python<'_>,
    obj: &Bound<'_, PyAny>,
) -> Option<Vec<RouteEntry>> {
    if let Ok(router) = obj.cast::<PyAPIRouter>() {
        return router.borrow().flatten(py).ok().map(|flat| flat.0.clone());
    }
    let routes = PyList::new(py, [obj]).ok()?;
    let router = PyAPIRouter::default();
    crate::ffi::openapi::add_route_objects(py, &router, routes.as_any()).ok()?;
    router.snapshot(py).ok().map(|flat| flat.0)
}
