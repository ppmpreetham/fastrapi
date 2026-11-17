//! The Python methods of the `FastrAPI` application class.
//! These methods add routes, mounts, middleware, exception handlers and routers to the app.
//! They also start the server and build the OpenAPI schema.

use super::config::{AsgiMount, FastrAPI, FrontendMount, HostMount, StaticMount, SubAppMount};
use super::server::serve::{serve, serve_with_reload};
use crate::engine::exception_dispatch::invalidate_handler_cache;
use crate::engine::server::asgi_app::new_asgi_cell;
use crate::ffi::decorators;
use crate::ffi::decorators::{PyAPIRouter, collect_router_routes, new_api_router};
use ahash::AHasher;
use fastrapi_core::method::HttpMethod;
use fastrapi_http::middleware::{DeclaredLayer, MiddlewareContainer, PyMiddleware};
use fastrapi_http::staticfiles::PyStaticFiles;
use fastrapi_openapi::spec::{AppDocument, build_openapi_spec};
use fastrapi_pyutil::imports::STARLETTE_STATE;
use fastrapi_runtime::tokio_runtime::set_threads;
use parking_lot::Mutex;
use pyo3::exceptions::{PyAssertionError, PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{IntoPyDict, PyAny, PyCFunction, PyDict, PyList, PyMapping, PyTuple};
use pyo3::{Bound, intern};
use std::env::var_os;
use std::hash::Hasher;
use std::num::NonZero;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

fastrapi_pyutil::cached_py_import!(STARLETTE_MIDDLEWARE, "starlette.middleware", "Middleware");
fastrapi_pyutil::cached_py_import!(URL_PATH, "starlette.datastructures", "URLPath");

fn child_name<'a>(mount_name: Option<&str>, name: &'a str) -> Option<&'a str> {
    match mount_name {
        None => Some(name),
        Some(mount_name) => name
            .strip_prefix(mount_name)
            .and_then(|rest| rest.strip_prefix(':')),
    }
}

fn mounted_file(
    mount_name: Option<&str>,
    name: &str,
    path_params: Option<&Bound<'_, PyDict>>,
) -> PyResult<Option<String>> {
    let Some(params) = path_params.filter(|params| params.len() == 1) else {
        return Ok(None);
    };
    if mount_name != Some(name) {
        return Ok(None);
    }
    params
        .get_item("path")?
        .map(|path| Ok(path.str()?.to_str()?.trim_start_matches('/').to_owned()))
        .transpose()
}
fastrapi_pyutil::cached_py_import!(
    STARLETTE_BASE_HTTP_MIDDLEWARE,
    "starlette.middleware.base",
    "BaseHTTPMiddleware"
);

fn is_startup_event(event_type: &str) -> PyResult<bool> {
    match event_type {
        "startup" => Ok(true),
        "shutdown" => Ok(false),
        _ => Err(PyAssertionError::new_err(())),
    }
}

fn telemetry_config(py: Python<'_>, overrides: Option<Bound<'_, PyAny>>) -> PyResult<Py<PyDict>> {
    let auto_configure = std::env::var("FASTAPI_OTEL_AUTO_CONFIGURE")
        .is_ok_and(|value| value.eq_ignore_ascii_case("true"));
    let none = py.None();
    let config = [
        ("tracer_provider", none.bind(py).clone()),
        ("meter_provider", none.bind(py).clone()),
        ("logger_provider", none.bind(py).clone()),
        ("tracing", true.into_pyobject(py)?.to_owned().into_any()),
        ("metrics", true.into_pyobject(py)?.to_owned().into_any()),
        ("logs", true.into_pyobject(py)?.to_owned().into_any()),
        (
            "operation_spans",
            true.into_pyobject(py)?.to_owned().into_any(),
        ),
        (
            "auto_configure",
            auto_configure.into_pyobject(py)?.to_owned().into_any(),
        ),
        ("exclude", none.bind(py).clone()),
    ]
    .into_py_dict(py)?;
    if let Some(overrides) = overrides {
        let Ok(mapping) = overrides.cast::<PyMapping>() else {
            return Err(PyTypeError::new_err(format!(
                "'{}' object is not a mapping",
                overrides.get_type().name()?
            )));
        };
        config.update(mapping)?;
    }
    Ok(config.unbind())
}

impl FastrAPI {
    pub(crate) fn apply_router_mounts(&mut self, py: Python<'_>) -> PyResult<()> {
        let root = self.router.bind(py).borrow().clone();
        let mut stack = vec![(root, String::new())];
        let mut mounts = Vec::new();
        let mut hosts = Vec::new();
        while let Some((router, prefix)) = stack.pop() {
            let full_prefix = fastrapi_core::path::join_path(&prefix, &router.prefix);
            for (path, app, name) in router.mounts.lock().drain(..) {
                let path = if full_prefix.is_empty() || full_prefix == "/" {
                    path
                } else {
                    fastrapi_core::path::join_path(&full_prefix, &path)
                };
                mounts.push((path, app, name));
            }
            hosts.extend(router.hosts.lock().drain(..));
            for mut frontend in router.frontends.lock().drain(..) {
                frontend.path = fastrapi_core::path::join_path(&full_prefix, &frontend.path);
                self.frontend_mounts.push(frontend);
            }
            for sub in router.sub_routers.lock().iter() {
                let sub_router = sub.router.bind(py).borrow().clone();
                stack.push((
                    sub_router,
                    fastrapi_core::path::join_path(&full_prefix, &sub.prefix),
                ));
            }
        }
        for (path, app, name) in mounts {
            self.mount(path, app.bind(py), name)?;
        }
        for (host, app, name) in hosts {
            self.host(py, host, app.bind(py), name)?;
        }
        Ok(())
    }

    fn mounted_url_path<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        path_params: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let url_path = URL_PATH.get(py)?;
        let prefixes = self
            .static_mounts
            .iter()
            .map(|mount| (mount.path.as_str(), mount.name.as_deref()))
            .chain(
                self.app_mounts
                    .iter()
                    .map(|mount| (mount.path.as_str(), mount.name.as_deref())),
            )
            .chain(
                self.asgi_mounts
                    .iter()
                    .map(|mount| (mount.path.as_str(), mount.name.as_deref())),
            );
        for (prefix, mount_name) in prefixes {
            if let Some(file) = mounted_file(mount_name, name, path_params)? {
                let path = format!("{}/{file}", prefix.trim_end_matches('/'));
                return url_path.call1((path,)).map(Some);
            }
        }
        let protocol = intern!(py, "protocol");
        for mount in &self.app_mounts {
            let Some(child) = child_name(mount.name.as_deref(), name) else {
                continue;
            };
            if let Ok(found) = mount.app.borrow(py).url_path_for(py, child, path_params) {
                let found = found.into_bound(py);
                let path = format!("{}{}", mount.path.trim_end_matches('/'), found.str()?);
                return url_path.call1((path, found.getattr(protocol)?)).map(Some);
            }
        }
        for mount in &self.host_mounts {
            let Some(child) = child_name(mount.name.as_deref(), name) else {
                continue;
            };
            if let Ok(found) = mount.app.borrow(py).url_path_for(py, child, path_params) {
                let found = found.into_bound(py);
                return url_path
                    .call1((found.str()?, found.getattr(protocol)?, &mount.host))
                    .map(Some);
            }
        }
        Ok(None)
    }

    fn push_event_handler(
        &mut self,
        py: Python<'_>,
        startup: bool,
        func: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let target = if startup {
            &mut self.on_startup
        } else {
            &mut self.on_shutdown
        };
        let list = match target.as_ref() {
            Some(value) => value.bind(py).cast::<PyList>()?.clone(),
            None => PyList::empty(py),
        };
        list.append(func)?;
        *target = Some(list.unbind().into_any());
        Ok(())
    }
}

pub(crate) fn frontend_mount(
    py: Python<'_>,
    mut path: String,
    directory: PathBuf,
    fallback: Option<String>,
    check_dir: CheckDir,
) -> PyResult<FrontendMount> {
    if !path.starts_with('/') {
        return Err(PyValueError::new_err("Frontend path must start with '/'"));
    }
    let check_dir = check_dir.resolve(py, &directory)?;
    if check_dir {
        let realpath = |path: &Path| -> PyResult<String> {
            py.import(intern!(py, "os.path"))?
                .call_method1(intern!(py, "realpath"), (path,))?
                .extract()
        };
        if !directory.is_dir() {
            return Err(PyRuntimeError::new_err(format!(
                "Frontend directory '{}' does not exist. Resolved absolute path: '{}'",
                directory.display(),
                realpath(&directory)?
            )));
        }
        if let Some(file) = fallback
            .as_deref()
            .filter(|file| matches!(*file, "index.html" | "404.html"))
            && !directory.join(file).is_file()
        {
            return Err(PyRuntimeError::new_err(format!(
                "Frontend fallback file '{file}' does not exist in directory '{}'. Resolved absolute directory: '{}'",
                directory.display(),
                realpath(&directory)?
            )));
        }
    }
    if path.len() > 1 && path.ends_with('/') {
        path.truncate(path.trim_end_matches('/').len());
    }
    Ok(FrontendMount {
        path,
        directory,
        fallback,
        check_dir,
    })
}

#[derive(FromPyObject)]
pub(crate) enum CheckDir {
    Flag(bool),
    Mode(String),
    #[pyo3(transparent)]
    Other(Py<PyAny>),
}

impl CheckDir {
    fn resolve(self, py: Python<'_>, directory: &Path) -> PyResult<bool> {
        let mode = match self {
            Self::Flag(flag) => return Ok(flag),
            Self::Other(value) => return value.bind(py).is_truthy(),
            Self::Mode(mode) => mode,
        };
        if mode != "auto" {
            return Ok(!mode.is_empty());
        }
        if std::env::var_os("FASTAPI_ENV").is_none_or(|env| env != "development") {
            return Ok(true);
        }
        if !directory.is_dir() {
            let realpath: String = py
                .import(intern!(py, "os.path"))?
                .call_method1(intern!(py, "realpath"), (directory,))?
                .extract()?;
            PyErr::warn(
                py,
                &py.get_type::<pyo3::exceptions::PyUserWarning>(),
                &std::ffi::CString::new(format!(
                    "Frontend directory '{}' does not exist. Resolved absolute path: '{realpath}'",
                    directory.display()
                ))?,
                2,
            )?;
        }
        Ok(false)
    }
}

#[pymethods]
impl FastrAPI {
    #[new]
    #[pyo3(signature = (
        *,
        debug=false,
        routes=None,
        title="FastAPI".to_string(),
        summary=None,
        description=String::new(),
        version="0.1.0".to_string(),
        openapi_url=Some("/openapi.json".to_string()),
        openapi_tags=None,
        servers=None,
        dependencies=None,
        default_response_class=None,
        redirect_slashes=true,
        docs_url=Some("/docs".to_string()),
        redoc_url=Some("/redoc".to_string()),
        scalar_url=None,
        elements_url=None,
        swagger_ui_oauth2_redirect_url=Some("/docs/oauth2-redirect".to_string()),
        swagger_ui_init_oauth=None,
        middleware=None,
        dependency_overrides=None,
        exception_handlers=None,
        on_startup=None,
        on_shutdown=None,
        lifespan=None,
        terms_of_service=None,
        contact=None,
        license_info=None,
        root_path=String::new(),
        root_path_in_servers=true,
        responses=None,
        callbacks=None,
        webhooks=None,
        deprecated=None,
        include_in_schema=true,
        swagger_ui_parameters=None,
        generate_unique_id_function=None,
        separate_input_output_schemas=true,
        openapi_external_docs=None,
        strict_content_type=None,
        sync_to_threadpool=true,
        max_body_size=Some(16 * 1024 * 1024),
        max_field_size=Some(1024 * 1024),
        max_file_size=Some(16 * 1024 * 1024),
        reject_unknown_multipart_fields=false,
        trace_requests=false,
        catch_panics=false,
        request_timeout=None,
        request_id_header=None,
        powered_by_header=None,
        telemetry=None,
    ))]
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "FastAPI constructor signature requires multiple configuration flags"
    )]
    fn new(
        py: Python<'_>,
        debug: bool,
        routes: Option<Py<PyAny>>,
        title: String,
        summary: Option<String>,
        description: String,
        version: String,
        openapi_url: Option<String>,
        openapi_tags: Option<Py<PyAny>>,
        servers: Option<Py<PyAny>>,
        dependencies: Option<Py<PyAny>>,
        default_response_class: Option<Py<PyAny>>,
        redirect_slashes: bool,
        docs_url: Option<String>,
        redoc_url: Option<String>,
        scalar_url: Option<String>,
        elements_url: Option<String>,
        swagger_ui_oauth2_redirect_url: Option<String>,
        swagger_ui_init_oauth: Option<Py<PyAny>>,
        middleware: Option<Py<PyAny>>,
        dependency_overrides: Option<Py<PyAny>>,
        exception_handlers: Option<Py<PyAny>>,
        on_startup: Option<Py<PyAny>>,
        on_shutdown: Option<Py<PyAny>>,
        lifespan: Option<Py<PyAny>>,
        terms_of_service: Option<String>,
        contact: Option<Py<PyAny>>,
        license_info: Option<Py<PyAny>>,
        root_path: String,
        root_path_in_servers: bool,
        responses: Option<Py<PyAny>>,
        callbacks: Option<Py<PyAny>>,
        webhooks: Option<Py<PyAny>>,
        deprecated: Option<bool>,
        include_in_schema: bool,
        swagger_ui_parameters: Option<Py<PyAny>>,
        generate_unique_id_function: Option<Py<PyAny>>,
        separate_input_output_schemas: bool,
        openapi_external_docs: Option<Py<PyAny>>,
        strict_content_type: Option<bool>,
        sync_to_threadpool: bool,
        max_body_size: Option<usize>,
        max_field_size: Option<usize>,
        max_file_size: Option<usize>,
        reject_unknown_multipart_fields: bool,
        trace_requests: bool,
        catch_panics: bool,
        request_timeout: Option<u64>,
        request_id_header: Option<String>,
        powered_by_header: Option<String>,
        telemetry: Option<Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        crate::ffi::decorators::tree::validate_dependencies(py, dependencies.as_ref())?;
        if let Some(url) = openapi_url.as_deref().filter(|url| !url.is_empty()) {
            if title.is_empty() {
                return Err(PyAssertionError::new_err(
                    "A title must be provided for OpenAPI, e.g.: 'My API'",
                ));
            }
            if version.is_empty() {
                return Err(PyAssertionError::new_err(
                    "A version must be provided for OpenAPI, e.g.: '2.1.0'",
                ));
            }
            crate::ffi::decorators::ensure_routed_path(url)?;
        }
        if !root_path.is_empty() {
            PyErr::warn(
                py,
                &py.get_type::<fastrapi_http::exceptions::PyFastAPIDeprecationWarning>(),
                c"The `root_path` parameter is deprecated. Configure the server's root path instead, for example with `fastapi run --root-path /api/v1`.",
                1,
            )?;
        }
        let explicit_default_response_class = default_response_class
            .as_ref()
            .map(|class| class.clone_ref(py));
        let default_response_class = match default_response_class {
            Some(c) => c,
            None => py
                .import(intern!(py, "fastrapi"))
                .and_then(|m| m.getattr(intern!(py, "responses")))
                .and_then(|r| r.getattr(intern!(py, "JSONResponse")))
                .map(Bound::unbind)?,
        };
        let generate_unique_id_function = generate_unique_id_function;
        let router_options = PyDict::new(py);
        router_options.set_item(
            intern!(py, "default_response_class"),
            explicit_default_response_class,
        )?;
        router_options.set_item(intern!(py, "strict_content_type"), strict_content_type)?;
        router_options.set_item(intern!(py, "deprecated"), deprecated)?;
        router_options.set_item(intern!(py, "include_in_schema"), include_in_schema)?;
        router_options.set_item(intern!(py, "callbacks"), callbacks.as_ref())?;
        let base_router = new_api_router(py, Some(&router_options))?.unbind();
        if let Some(routes) = &routes {
            crate::ffi::openapi::add_route_objects(py, &base_router.borrow(py), routes.bind(py))?;
        }

        let state = STARLETTE_STATE
            .get(py)
            .and_then(|cls| cls.call0().map(Bound::unbind))
            .or_else(|_| {
                py.import(pyo3::intern!(py, "types"))
                    .and_then(|types| types.getattr(pyo3::intern!(py, "SimpleNamespace")))
                    .and_then(|ns| ns.call0().map(Bound::unbind))
            })
            .unwrap_or_else(|_| py.None());

        Ok(Self {
            debug,
            state,
            routes,
            title,
            summary,
            description,
            version,
            openapi_url,
            openapi_tags,
            servers,
            dependencies,
            default_response_class,
            redirect_slashes,
            docs_url,
            redoc_url,
            scalar_url,
            elements_url,
            swagger_ui_oauth2_redirect_url,
            swagger_ui_init_oauth,
            user_middleware: match &middleware {
                Some(middleware) => PyList::new(
                    py,
                    middleware
                        .bind(py)
                        .try_iter()?
                        .collect::<PyResult<Vec<_>>>()?,
                )?
                .unbind(),
                None => PyList::empty(py).unbind(),
            },
            middleware,
            dependency_overrides: dependency_overrides.or_else(|| Some(PyDict::new(py).into())),
            exception_handlers: Some({
                let registry = PyDict::new(py);
                if let Some(handlers) = exception_handlers {
                    registry.call_method1(pyo3::intern!(py, "update"), (handlers,))?;
                }
                fastrapi_http::exceptions::register_default_handlers(&registry)?;
                registry.into_any().unbind()
            }),
            on_startup,
            on_shutdown,
            lifespan,
            terms_of_service,
            contact,
            license_info,
            root_path,
            root_path_in_servers,
            responses,
            callbacks,
            webhooks: match webhooks {
                Some(webhooks) => Some(webhooks),
                None => Some(new_api_router(py, None)?.into_any().unbind()),
            },
            deprecated,
            include_in_schema,
            swagger_ui_parameters,
            generate_unique_id_function,
            separate_input_output_schemas,
            openapi_external_docs,
            sync_to_threadpool,
            max_body_size,
            max_field_size,
            max_file_size,
            reject_unknown_multipart_fields,
            trace_requests,
            catch_panics,
            request_timeout,
            request_id_header,
            powered_by_header,
            static_mounts: Vec::new(),
            frontend_mounts: Vec::new(),
            app_mounts: Vec::new(),
            asgi_mounts: Vec::new(),
            host_mounts: Vec::new(),
            prometheus_config: None,
            middlewares: MiddlewareContainer::default(),
            openapi_cache: Mutex::new(None),
            openapi_schema: Mutex::new(None),
            asgi: new_asgi_cell(),
            stack_override: None,
            docs_routes: pyo3::sync::PyOnceLock::new(),
            openapi_version: "3.1.0".to_string(),
            telemetry: telemetry_config(py, telemetry)?,
            router: base_router,
        })
    }

    #[pyo3(signature = (middleware_class, *args, **kwargs))]
    fn add_middleware(
        &mut self,
        middleware_class: &Bound<'_, PyAny>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        if self.asgi.get(middleware_class.py()).is_some() || self.stack_override.is_some() {
            return Err(PyRuntimeError::new_err(
                "Cannot add middleware after an application has started",
            ));
        }
        let py = middleware_class.py();
        let empty_dict;
        let opts = if let Some(dict) = kwargs {
            dict
        } else {
            empty_dict = PyDict::new(py);
            &empty_dict
        };
        self.middlewares.register(middleware_class, args, opts)?;
        let positional = PyTuple::new(
            py,
            std::iter::once(middleware_class.clone())
                .chain(args)
                .collect::<Vec<_>>(),
        )?;
        let entry = STARLETTE_MIDDLEWARE.get(py)?.call(positional, Some(opts))?;
        self.user_middleware.bind(py).insert(0, entry)
    }

    #[pyo3(signature = (path))]
    fn const_get(&self, py: Python<'_>, path: String) -> PyResult<Py<PyAny>> {
        self.router
            .bind(py)
            .borrow()
            .create_method_decorator_kw(py, HttpMethod::GET, path, None)
    }

    /// Decorate a WebSocket function.
    ///
    /// Read more about it in the
    /// [FastAPI docs for WebSockets](https://fastapi.tiangolo.com/advanced/websockets/).
    ///
    /// **Example**
    ///
    /// ```python
    /// from fastrapi import FastAPI, WebSocket
    ///
    /// app = FastAPI()
    ///
    /// @app.websocket("/ws")
    /// async def websocket_endpoint(websocket: WebSocket):
    ///     await websocket.accept()
    ///     while True:
    ///         data = await websocket.receive_text()
    ///         await websocket.send_text(f"Message text was: {data}")
    /// ```
    #[pyo3(signature = (path, name=None, *, dependencies=None))]
    fn websocket(
        &self,
        py: Python<'_>,
        path: String,
        name: Option<String>,
        dependencies: Option<Py<PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        self.router
            .bind(py)
            .borrow()
            .create_ws_decorator(py, path, name, dependencies)
    }

    #[pyo3(signature = (path, app, name=None))]
    fn mount(
        &mut self,
        path: String,
        app: &Bound<'_, PyAny>,
        name: Option<String>,
    ) -> PyResult<()> {
        crate::ffi::decorators::ensure_routed_path(&path)?;

        let normalized_path = if path.len() > 1 {
            path.trim_end_matches('/').to_string()
        } else {
            path
        };

        if app.extract::<PyRef<'_, Self>>().is_ok() {
            let owned = app
                .clone()
                .into_any()
                .cast_into::<Self>()
                .map_err(PyErr::from)?
                .unbind();
            self.app_mounts.push(SubAppMount {
                path: normalized_path,
                app: owned,
                name,
            });
            return Ok(());
        }

        if let Ok(static_files) = app.extract::<PyRef<'_, PyStaticFiles>>() {
            self.static_mounts.push(StaticMount {
                path: normalized_path,
                directory: static_files.directory.clone(),
                directories: static_files.directories.clone(),
                html: static_files.html,
                follow_symlink: static_files.follow_symlink,
                name,
            });
            return Ok(());
        }
        if !app.is_callable() {
            return Err(PyTypeError::new_err(
                "mounted app must be an ASGI application",
            ));
        }
        self.asgi_mounts.push(AsgiMount {
            path: normalized_path,
            app: Arc::new(app.clone().unbind()),
            name,
        });
        Ok(())
    }

    /// Add an event handler for the application.
    ///
    /// `on_event` is deprecated, use `lifespan` event handlers instead.
    ///
    /// Read more about it in the
    /// [FastAPI docs for Lifespan Events](https://fastapi.tiangolo.com/advanced/events/#alternative-events-deprecated).
    fn on_event(slf: Py<Self>, py: Python<'_>, event_type: &str) -> PyResult<Py<PyAny>> {
        crate::ffi::decorators::warn_on_event_deprecated(py)?;
        let event_type = event_type.to_owned();
        let decorator = move |args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>| {
            let startup = is_startup_event(&event_type)?;
            let func = args.get_item(0)?.unbind();
            Python::attach(|py| -> PyResult<Py<PyAny>> {
                slf.borrow_mut(py)
                    .push_event_handler(py, startup, func.bind(py))?;
                Ok(func)
            })
        };
        PyCFunction::new_closure(py, Some(c"on_event"), None, decorator).map(Into::into)
    }

    fn add_event_handler(
        &mut self,
        py: Python<'_>,
        event_type: &str,
        func: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let startup = is_startup_event(event_type)?;
        self.push_event_handler(py, startup, func)
    }

    #[pyo3(signature = (host, app, *, name=None))]
    fn host(
        &mut self,
        _py: Python<'_>,
        host: String,
        app: &Bound<'_, PyAny>,
        name: Option<String>,
    ) -> PyResult<()> {
        if host.starts_with('/') {
            return Err(PyValueError::new_err("Host must not start with '/'"));
        }
        let owned = app
            .clone()
            .into_any()
            .cast_into::<Self>()
            .map_err(PyErr::from)?
            .unbind();
        self.host_mounts.push(HostMount {
            host,
            app: owned,
            name,
        });
        Ok(())
    }

    fn add_exception_handler(
        &self,
        py: Python<'_>,
        exc_class_or_status_code: Py<PyAny>,
        handler: Py<PyAny>,
    ) -> PyResult<()> {
        if let Some(handlers) = &self.exception_handlers {
            handlers
                .bind(py)
                .set_item(exc_class_or_status_code, handler)?;
            invalidate_handler_cache();
        }
        Ok(())
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
        let route_kwargs = crate::ffi::decorators::add_api_route_kwargs(py, kwargs)?;
        self.router
            .bind(py)
            .borrow()
            .add_api_route_impl(py, path, endpoint, methods, &route_kwargs)
    }

    #[getter(routes)]
    fn get_routes<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyList>> {
        let py = slf.py();
        let app = slf.borrow();
        let docs = app.docs_routes.get_or_try_init(py, || {
            crate::ffi::compat::py_helpers(py)?
                .getattr(intern!(py, "docs_routes"))?
                .call1((slf,))?
                .cast_into::<PyList>()
                .map(Bound::unbind)
                .map_err(PyErr::from)
        })?;
        let all = PyList::new(py, docs.bind(py))?;
        let (routes, websockets) = app.router.bind(py).borrow().snapshot(py)?;
        for route in crate::ffi::routing::route_objects(py, &routes, &websockets)? {
            route.setattr(intern!(py, "dependency_overrides_provider"), slf)?;
            all.append(route)?;
        }
        Ok(all)
    }

    #[pyo3(signature = (name, /, **path_params))]
    fn url_path_for(
        &self,
        py: Python<'_>,
        name: &str,
        path_params: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let routed = self
            .router
            .bind(py)
            .borrow()
            .url_path_for_name(py, name, path_params);
        match routed {
            Err(err) => self
                .mounted_url_path(py, name, path_params)?
                .map(Bound::unbind)
                .ok_or(err),
            found => found,
        }
    }

    #[pyo3(signature = (path, methods=None, **kwargs))]
    fn api_route(
        &self,
        py: Python<'_>,
        path: &str,
        methods: Option<Vec<String>>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        self.router
            .bind(py)
            .borrow()
            .create_api_route_decorator(py, path, methods, kwargs)
    }

    /// Serve a static frontend build as low-priority routes.
    ///
    /// Use this for frontend tools that build static files into a directory,
    /// such as `dist`. **FastAPI** path operations are checked first, and
    /// the frontend files are checked only if no normal route matched.
    ///
    /// A typical project could look like this:
    ///
    /// ```text
    /// .
    /// ├── pyproject.toml
    /// ├── app
    /// │   ├── __init__.py
    /// │   └── main.py
    /// └── dist
    ///     ├── index.html
    ///     └── assets
    ///         └── app.js
    /// ```
    ///
    /// Then in `app/main.py`:
    ///
    /// ```python
    /// from fastrapi import FastAPI
    ///
    /// app = FastAPI()
    /// app.frontend("/", directory="dist")
    /// ```
    #[pyo3(
        signature = (path, *, directory, fallback=Some("auto".to_string()), check_dir=CheckDir::Mode("auto".to_owned())),
        text_signature = "(self, path, *, directory, fallback='auto', check_dir='auto')"
    )]
    fn frontend(
        &mut self,
        py: Python<'_>,
        path: String,
        directory: PathBuf,
        fallback: Option<String>,
        check_dir: CheckDir,
    ) -> PyResult<()> {
        self.frontend_mounts
            .push(frontend_mount(py, path, directory, fallback, check_dir)?);
        Ok(())
    }

    /// Add a middleware to the application.
    ///
    /// Read more about it in the
    /// [FastAPI docs for Middleware](https://fastapi.tiangolo.com/tutorial/middleware/).
    ///
    /// ## Example
    ///
    /// ```python
    /// import time
    /// from typing import Awaitable, Callable
    ///
    /// from fastrapi import FastAPI, Request, Response
    ///
    /// app = FastAPI()
    ///
    ///
    /// @app.middleware("http")
    /// async def add_process_time_header(
    ///     request: Request, call_next: Callable[[Request], Awaitable[Response]]
    /// ) -> Response:
    ///     start_time = time.time()
    ///     response = await call_next(request)
    ///     process_time = time.time() - start_time
    ///     response.headers["X-Process-Time"] = str(process_time)
    ///     return response
    /// ```
    fn middleware(slf: Py<Self>, py: Python<'_>, middleware_type: String) -> PyResult<Py<PyAny>> {
        let decorator = move |args: &Bound<'_, PyTuple>,
                              _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> {
            let py = args.py();
            let func: Py<PyAny> = args.get_item(0)?.unbind();
            let py_middleware = PyMiddleware::new(py, func.clone_ref(py));
            let entry = STARLETTE_MIDDLEWARE.get(py)?.call(
                (STARLETTE_BASE_HTTP_MIDDLEWARE.get(py)?,),
                Some(&[("dispatch", func.bind(py))].into_py_dict(py)?),
            )?;
            let mut app = slf.borrow_mut(py);
            app.user_middleware.bind(py).insert(0, entry)?;
            app.middlewares
                .order
                .push(DeclaredLayer::Decorator(Arc::new(py_middleware)));
            info!("🔗 Registered {} middleware", middleware_type);
            Ok(func)
        };
        PyCFunction::new_closure(
            py,
            Some(c"middleware"),
            Some(c"Registers middleware of given type"),
            decorator,
        )
        .map(Into::into)
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "PyO3 pymethod receiver requires Py<Self>"
    )]
    #[pyo3(signature = (host=None, port=None, *, threads=None, reload=false, reload_dirs=None, reload_ignore_dirs=None, reload_ignore_patterns=None, reload_ignore_paths=None, reload_tick=750, reload_ignore_worker_failure=false))]
    fn serve(
        slf: Py<Self>,
        py: Python,
        host: Option<String>,
        port: Option<u16>,
        threads: Option<usize>,
        reload: bool,
        reload_dirs: Option<Vec<String>>,
        reload_ignore_dirs: Option<Vec<String>>,
        reload_ignore_patterns: Option<Vec<String>>,
        reload_ignore_paths: Option<Vec<String>>,
        reload_tick: u64,
        reload_ignore_worker_failure: bool,
    ) -> PyResult<()> {
        if let Some(threads) = threads {
            let threads = NonZero::new(threads)
                .ok_or_else(|| PyValueError::new_err("threads must be at least 1"))?;
            set_threads(threads).map_err(PyRuntimeError::new_err)?;
        }

        if reload && var_os("FASTRAPI_RELOAD_CHILD").is_none() {
            serve_with_reload(
                py,
                reload_dirs,
                reload_ignore_dirs,
                reload_ignore_patterns,
                reload_ignore_paths,
                reload_tick,
                reload_ignore_worker_failure,
            )
        } else {
            serve(py, host, port, &slf)
        }
    }

    #[getter]
    fn middleware_stack(slf: &Bound<'_, Self>) -> PyResult<Option<Py<PyAny>>> {
        let app = slf.borrow();
        if let Some(stack) = &app.stack_override {
            return Ok(Some(stack.clone_ref(slf.py())));
        }
        if app.asgi.get(slf.py()).is_none() {
            return Ok(None);
        }
        slf.getattr(intern!(slf.py(), "_asgi"))
            .map(|stack| Some(stack.unbind()))
    }

    #[setter]
    fn set_middleware_stack(&mut self, stack: Option<Py<PyAny>>) {
        if stack.is_none() {
            self.asgi = new_asgi_cell();
        }
        self.stack_override = stack;
    }

    #[getter]
    fn _middleware_stack_override(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        self.stack_override
            .as_ref()
            .map(|stack| stack.clone_ref(py))
    }

    fn build_middleware_stack<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        slf.borrow_mut().asgi = new_asgi_cell();
        slf.getattr(intern!(slf.py(), "_asgi"))
    }

    const fn setup(&self) {}

    fn _asgi<'py>(
        slf: &Bound<'py, Self>,
        scope: &Bound<'py, PyDict>,
        receive: Bound<'py, PyAny>,
        send: Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        crate::engine::server::asgi_app::call(slf, scope, receive, send)
    }

    /// Include an `APIRouter` in the same app.
    ///
    /// Read more about it in the
    /// [FastAPI docs for Bigger Applications](https://fastapi.tiangolo.com/tutorial/bigger-applications/).
    ///
    /// ## Example
    ///
    /// ```python
    /// from fastrapi import FastAPI
    ///
    /// from .users import users_router
    ///
    /// app = FastAPI()
    ///
    /// app.include_router(users_router)
    /// ```
    #[pyo3(signature = (router, *, prefix="", tags=None, dependencies=None, responses=None, deprecated=None, include_in_schema=true, default_response_class=None, callbacks=None, generate_unique_id_function=None))]
    fn include_router(
        &self,
        router: &Bound<'_, PyAPIRouter>,
        prefix: &str,
        tags: Option<&Bound<'_, pyo3::PyAny>>,
        dependencies: Option<&Bound<'_, pyo3::PyAny>>,
        responses: Option<&Bound<'_, pyo3::PyAny>>,
        deprecated: Option<bool>,
        include_in_schema: bool,
        default_response_class: Option<&Bound<'_, pyo3::PyAny>>,
        callbacks: Option<&Bound<'_, pyo3::PyAny>>,
        generate_unique_id_function: Option<&Bound<'_, pyo3::PyAny>>,
    ) -> PyResult<()> {
        self.router.bind(router.py()).borrow().include_router(
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

    #[pyo3(signature = (prefix, router, *, tags=None, dependencies=None, responses=None, deprecated=None, include_in_schema=true, default_response_class=None, callbacks=None, generate_unique_id_function=None))]
    fn nest(
        &self,
        prefix: &str,
        router: &Bound<'_, PyAPIRouter>,
        tags: Option<&Bound<'_, pyo3::PyAny>>,
        dependencies: Option<&Bound<'_, pyo3::PyAny>>,
        responses: Option<&Bound<'_, pyo3::PyAny>>,
        deprecated: Option<bool>,
        include_in_schema: bool,
        default_response_class: Option<&Bound<'_, pyo3::PyAny>>,
        callbacks: Option<&Bound<'_, pyo3::PyAny>>,
        generate_unique_id_function: Option<&Bound<'_, pyo3::PyAny>>,
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

    /// Generate the OpenAPI schema of the application. This is called by FastAPI
    /// internally.
    ///
    /// The first time it is called it stores the result in the attribute
    /// `app.openapi_schema`, and next times it is called, it just returns that same
    /// result. To avoid the cost of generating the schema every time.
    ///
    /// If you need to modify the generated OpenAPI schema, you could modify it.
    ///
    /// Read more in the
    /// [FastAPI docs for OpenAPI](https://fastapi.tiangolo.com/how-to/extending-openapi/).
    #[pyo3(signature = ())]
    fn openapi(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        if let Some(schema) = self.openapi_schema.lock().as_ref() {
            return Ok(schema.clone_ref(py));
        }
        let fingerprint = self.openapi_fingerprint(py);
        if let Some(entry) = self.openapi_cache.lock().as_ref()
            && entry.fingerprint == fingerprint
            && entry.openapi_version == self.openapi_version
            && entry.title == self.title
            && entry.version == self.version
            && entry.description == self.description
        {
            return Ok(entry.dict.clone_ref(py));
        }

        let spec = self.openapi_spec(py, false)?;
        let dict = fastrapi_pyutil::json::json_value_to_py(py, &spec)?;
        *self.openapi_cache.lock() = Some(super::config::OpenApiCacheEntry {
            fingerprint,
            openapi_version: self.openapi_version.clone(),
            title: self.title.clone(),
            version: self.version.clone(),
            description: self.description.clone(),
            dict: dict.clone().unbind(),
        });
        Ok(dict.unbind())
    }

    #[getter]
    fn get_openapi_schema(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        if let Some(schema) = self.openapi_schema.lock().as_ref() {
            return Some(schema.clone_ref(py));
        }
        self.openapi_cache
            .lock()
            .as_ref()
            .map(|entry| entry.dict.clone_ref(py))
    }

    #[setter]
    fn set_openapi_schema(&self, schema: Option<Py<PyAny>>) {
        *self.openapi_schema.lock() = schema;
    }

    /// Add an exception handler to the app.
    ///
    /// Read more about it in the
    /// [FastAPI docs for Handling Errors](https://fastapi.tiangolo.com/tutorial/handling-errors/).
    ///
    /// ## Example
    ///
    /// ```python
    /// from fastrapi import FastAPI, Request
    /// from fastrapi.responses import JSONResponse
    ///
    ///
    /// class UnicornException(Exception):
    ///     def __init__(self, name: str):
    ///         self.name = name
    ///
    ///
    /// app = FastAPI()
    ///
    ///
    /// @app.exception_handler(UnicornException)
    /// async def unicorn_exception_handler(request: Request, exc: UnicornException):
    ///     return JSONResponse(
    ///         status_code=418,
    ///         content={"message": f"Oops! {exc.name} did something. There goes a rainbow..."},
    ///     )
    /// ```
    fn exception_handler(
        &self,
        py: Python,
        exc_class_or_status_code: Py<PyAny>,
    ) -> PyResult<Py<PyAny>> {
        let exception_handlers = self.exception_handlers.clone();
        let decorator = move |args: &Bound<'_, PyTuple>,
                              _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> {
            let py = args.py();
            let func: Py<PyAny> = args.get_item(0)?.unbind();
            if let Some(handlers) = &exception_handlers {
                handlers
                    .bind(py)
                    .set_item(exc_class_or_status_code.clone_ref(py), func.clone_ref(py))?;
                invalidate_handler_cache();
            }
            Ok(func)
        };
        PyCFunction::new_closure(py, Some(c"exception_handler"), None, decorator).map(Into::into)
    }

    fn fallback(&self, py: Python, handler: Py<PyAny>) -> PyResult<Py<PyAny>> {
        if let Some(handlers) = &self.exception_handlers {
            handlers.bind(py).set_item(404, handler.clone_ref(py))?;
            invalidate_handler_cache();
        }
        Ok(handler)
    }
}

impl FastrAPI {
    #[inline]
    fn router_ref<'py>(&self, py: Python<'py>) -> pyo3::PyRef<'py, decorators::PyAPIRouter> {
        self.router.bind(py).borrow()
    }

    fn openapi_fingerprint(&self, py: Python<'_>) -> u64 {
        let mut h = AHasher::default();
        self.hash_openapi_state(py, &mut h, 0);
        h.finish()
    }

    fn hash_openapi_state(&self, py: Python<'_>, h: &mut AHasher, depth: u8) {
        let router = self.router.bind(py);
        let Ok(router) = router.try_borrow() else {
            h.write_u8(0xFF);
            return;
        };
        h.write_u64(router.tree_revision(py));
        h.write_usize(router.route_entries.lock().len());
        h.write_usize(router.websocket_entries.lock().len());
        h.write_usize(router.sub_routers.lock().len());
        h.write_usize(self.static_mounts.len());
        h.write_usize(self.frontend_mounts.len());
        h.write_usize(self.app_mounts.len());

        if depth < 8 {
            for mount in &self.app_mounts {
                if let Ok(sub) = mount.app.bind(py).try_borrow() {
                    sub.hash_openapi_state(py, h, depth + 1);
                }
            }
        }
    }
}
crate::generate_http_methods!(
    FastrAPI,
    class_name: "FastAPI",
    router_ref,
    route_arg: route,
    websocket_arg: route,
    get: {
        /// Add a *path operation* using an HTTP GET operation.
        ///
        /// ## Example
        ///
        /// ```python
        /// from fastrapi import FastAPI
        ///
        /// app = FastAPI()
        ///
        /// @app.get("/items/")
        /// def read_items():
        ///     return [{"name": "Empanada"}, {"name": "Arepa"}]
        /// ```
    },
    post: {
        /// Add a *path operation* using an HTTP POST operation.
        ///
        /// ## Example
        ///
        /// ```python
        /// from fastrapi import FastAPI
        /// from pydantic import BaseModel
        ///
        /// class Item(BaseModel):
        ///     name: str
        ///     description: str | None = None
        ///
        /// app = FastAPI()
        ///
        /// @app.post("/items/")
        /// def create_item(item: Item):
        ///     return {"message": "Item created"}
        /// ```
    },
    put: {
        /// Add a *path operation* using an HTTP PUT operation.
        ///
        /// ## Example
        ///
        /// ```python
        /// from fastrapi import FastAPI
        /// from pydantic import BaseModel
        ///
        /// class Item(BaseModel):
        ///     name: str
        ///     description: str | None = None
        ///
        /// app = FastAPI()
        ///
        /// @app.put("/items/{item_id}")
        /// def replace_item(item_id: str, item: Item):
        ///     return {"message": "Item replaced", "id": item_id}
        /// ```
    },
    delete: {
        /// Add a *path operation* using an HTTP DELETE operation.
        ///
        /// ## Example
        ///
        /// ```python
        /// from fastrapi import FastAPI
        ///
        /// app = FastAPI()
        ///
        /// @app.delete("/items/{item_id}")
        /// def delete_item(item_id: str):
        ///     return {"message": "Item deleted"}
        /// ```
    },
    patch: {
        /// Add a *path operation* using an HTTP PATCH operation.
        ///
        /// ## Example
        ///
        /// ```python
        /// from fastrapi import FastAPI
        /// from pydantic import BaseModel
        ///
        /// class Item(BaseModel):
        ///     name: str
        ///     description: str | None = None
        ///
        /// app = FastAPI()
        ///
        /// @app.patch("/items/")
        /// def update_item(item: Item):
        ///     return {"message": "Item updated in place"}
        /// ```
    },
    options: {
        /// Add a *path operation* using an HTTP OPTIONS operation.
        ///
        /// ## Example
        ///
        /// ```python
        /// from fastrapi import FastAPI
        ///
        /// app = FastAPI()
        ///
        /// @app.options("/items/")
        /// def get_item_options():
        ///     return {"additions": ["Aji", "Guacamole"]}
        /// ```
    },
    head: {
        /// Add a *path operation* using an HTTP HEAD operation.
        ///
        /// ## Example
        ///
        /// ```python
        /// from fastrapi import FastAPI, Response
        ///
        /// app = FastAPI()
        ///
        /// @app.head("/items/", status_code=204)
        /// def get_items_headers(response: Response):
        ///     response.headers["X-Cat-Dog"] = "Alone in the world"
        /// ```
    },
);

impl FastrAPI {
    /// The `/openapi.json` route adds the `root_path` server, but a direct `openapi()` call does not.
    pub(crate) fn openapi_spec(
        &self,
        py: Python<'_>,
        with_root_path_server: bool,
    ) -> PyResult<simd_json::OwnedValue> {
        let flat = self.router.bind(py).borrow().flatten(py)?;
        let routes = &flat.0;
        let document = AppDocument {
            openapi_version: &self.openapi_version,
            title: &self.title,
            version: &self.version,
            summary: self.summary.as_deref(),
            description: &self.description,
            terms_of_service: self.terms_of_service.as_deref(),
            contact: self.contact.as_ref(),
            license_info: self.license_info.as_ref(),
            servers: self.servers.as_ref(),
            root_path_in_servers: self.root_path_in_servers && with_root_path_server,
            root_path: &self.root_path,
            openapi_tags: self.openapi_tags.as_ref(),
            openapi_external_docs: self.openapi_external_docs.as_ref(),
            responses: self.responses.as_ref(),
            webhooks: self.webhooks.as_ref(),
            generate_unique_id_function: self.generate_unique_id_function.as_ref(),
            separate_input_output_schemas: self.separate_input_output_schemas,
            routes,
        };
        build_openapi_spec(py, &document, collect_router_routes)
    }
}
