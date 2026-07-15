use pyo3::exceptions::PyValueError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyString, PyTuple};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tracing::info;

use super::server;
pub use super::types::{FastrAPI, FrontendMount, StaticMount};
use crate::decorators::PyAPIRouter;
use crate::globals::{MIDDLEWARE_COUNTER, MIDDLEWARES};
use crate::http::middleware::{
    PyMiddleware, parse_cors_params, parse_gzip_params, parse_session_params,
    parse_trusted_host_params,
};
use crate::http::staticfiles::PyStaticFiles;
use crate::routing::types::HttpMethod;

#[pymethods]
impl FastrAPI {
    #[new]
    #[pyo3(signature = (
        *,
        debug=false,
        routes=None,
        title="FastrAPI".to_string(),
        summary=None,
        description="".to_string(),
        version="0.1.0".to_string(),
        openapi_url="/api-docs/openapi.json".to_string(),
        openapi_tags=None,
        servers=None,
        dependencies=None,
        default_response_class=None,
        redirect_slashes=true,
        docs_url=Some("/docs".to_string()),
        redoc_url=Some("/redoc".to_string()),
        scalar_url=Some("/scalar".to_string()),
        elements_url=Some("/elements".to_string()),
        swagger_ui_oauth2_redirect_url=Some("/docs/oauth2-redirect".to_string()),
        swagger_ui_init_oauth=None,
        middleware=None,
        exception_handlers=None,
        on_startup=None,
        on_shutdown=None,
        lifespan=None,
        terms_of_service=None,
        contact=None,
        license_info=None,
        openapi_prefix="".to_string(),
        root_path="".to_string(),
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
        sync_to_threadpool=false,
        max_body_size=Some(16 * 1024 * 1024),
        max_field_size=Some(1024 * 1024),
        max_file_size=Some(16 * 1024 * 1024),
        reject_unknown_multipart_fields=false,
        trace_requests=false,
        catch_panics=false,
        request_timeout=None,
        request_id_header=None,
        powered_by_header=None,
    ))]
    fn new(
        py: Python<'_>,
        debug: bool,
        routes: Option<Py<PyAny>>,
        title: String,
        summary: Option<String>,
        description: String,
        version: String,
        openapi_url: String,
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
        exception_handlers: Option<Py<PyAny>>,
        on_startup: Option<Py<PyAny>>,
        on_shutdown: Option<Py<PyAny>>,
        lifespan: Option<Py<PyAny>>,
        terms_of_service: Option<String>,
        contact: Option<Py<PyAny>>,
        license_info: Option<Py<PyAny>>,
        openapi_prefix: String,
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
    ) -> PyResult<Self> {
        let default_response_class = default_response_class.unwrap_or_else(|| {
            py.import(intern!(py, "fastrapi"))
                .and_then(|m| m.getattr(intern!(py, "responses")))
                .and_then(|r| r.getattr(intern!(py, "JSONResponse")))
                .map(|obj| obj.unbind())
                .unwrap()
        });
        let generate_unique_id_function = match generate_unique_id_function {
            Some(func) => func,
            None => py
                .eval(c"lambda route: route.__name__", None, None)?
                .unbind(),
        };
        let base_router = Py::new(py, PyAPIRouter::new_())?;

        Ok(Self {
            debug,
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
            middleware,
            exception_handlers: exception_handlers.or_else(|| Some(PyDict::new(py).into())),
            on_startup,
            on_shutdown,
            lifespan,
            terms_of_service,
            contact,
            license_info,
            openapi_prefix,
            root_path,
            root_path_in_servers,
            responses,
            callbacks,
            webhooks,
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
            prometheus_config: None,
            cors_config: None,
            trusted_host_config: None,
            gzip_config: None,
            session_config: None,
            router: base_router,
        })
    }

    #[pyo3(signature = (middleware_class, **kwargs))]
    fn add_middleware(
        &mut self,
        py: Python,
        middleware_class: Py<PyAny>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        let class_name_obj = middleware_class.bind(py).getattr(intern!(py, "__name__"))?;
        let class_name = class_name_obj.cast::<PyString>()?.to_str()?.to_owned();

        let opts = &kwargs.cloned().unwrap_or_else(|| PyDict::new(py));
        match class_name.as_str() {
            "CORSMiddleware" => {
                self.cors_config = Some(parse_cors_params(opts)?);
                info!("Enabled CORSMiddleware");
            }
            "TrustedHostMiddleware" => {
                self.trusted_host_config = Some(parse_trusted_host_params(opts)?);
                info!("Enabled TrustedHostMiddleware");
            }
            "GZipMiddleware" => {
                self.gzip_config = Some(parse_gzip_params(opts)?);
                info!("Enabled GZipMiddleware");
            }
            "SessionMiddleware" => {
                self.session_config = Some(parse_session_params(opts)?);
                info!("Enabled SessionMiddleware");
            }
            _ => {
                let msg = format!(
                    "Middleware '{}' is not supported. Only CORSMiddleware, TrustedHostMiddleware, GZipMiddleware, and SessionMiddleware are allowed via add_middleware.",
                    class_name
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
        }
        Ok(())
    }


    #[pyo3(signature = (path))]
    fn const_get(&self, py: Python<'_>, path: String) -> PyResult<Py<PyAny>> {
        self.router.bind(py).borrow().create_method_decorator(
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
            None,
        )
    }

    #[pyo3(signature = (path))]
    fn websocket(&self, py: Python<'_>, path: String) -> PyResult<Py<PyAny>> {
        self.router.bind(py).borrow().create_ws_decorator(py, path)
    }

    #[pyo3(signature = (path, app, *, name=None))]
    fn mount(
        &mut self,
        py: Python<'_>,
        path: String,
        app: Py<PyAny>,
        name: Option<String>,
    ) -> PyResult<()> {
        if !path.starts_with('/') {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "Mount path must start with '/'",
            ));
        }

        let static_files = app.bind(py).extract::<PyRef<'_, PyStaticFiles>>()?;
        let normalized_path = if path.len() > 1 {
            path.trim_end_matches('/').to_string()
        } else {
            path
        };

        self.static_mounts.push(StaticMount {
            path: normalized_path,
            directory: static_files.directory.clone(),
            html: static_files.html,
            follow_symlink: static_files.follow_symlink,
            name,
        });
        Ok(())
    }

    #[pyo3(signature = (path, *, directory, fallback=Some("auto".to_string()), check_dir=true))]
    fn frontend(
        &mut self,
        mut path: String,
        directory: String,
        fallback: Option<String>,
        check_dir: bool,
    ) -> PyResult<()> {
        if !path.starts_with('/') {
            return Err(PyValueError::new_err("Frontend path must start with '/'"));
        }

        if check_dir && !Path::new(&directory).is_dir() {
            return Err(PyValueError::new_err(format!(
                "Directory '{directory}' does not exist"
            )));
        }

        if path.len() > 1 && path.ends_with('/') {
            path.truncate(path.trim_end_matches('/').len());
        }

        self.frontend_mounts.push(FrontendMount {
            path,
            directory,
            fallback,
            check_dir,
        });

        Ok(())
    }

    // decorator for generic Python functions: @app.middleware("smtg")
    fn middleware(&self, py: Python, middleware_type: String) -> PyResult<Py<PyAny>> {
        let decorator = move |args: &Bound<'_, PyTuple>,
                              _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> {
            let py = args.py();
            let func: Py<PyAny> = args.get_item(0)?.unbind(); // 0th item is the function being decorated
            let py_middleware = PyMiddleware::new(func.clone_ref(py));
            let id = MIDDLEWARE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let middleware_id = format!("{}_{}", middleware_type, id);
            MIDDLEWARES
                .pin()
                .insert(middleware_id.clone(), Arc::new(py_middleware));
            info!("🔗 Registered middleware: {}", middleware_id);
            Ok(func)
        };
        PyCFunction::new_closure(
            py,
            Some(c"middleware"),
            Some(c"Registers middleware of given type"),
            decorator,
        )
        .map(|f| f.into())
    }

    #[pyo3(signature = (host=None, port=None, *, reload=false, reload_dirs=None, reload_ignore_dirs=None, reload_ignore_patterns=None, reload_ignore_paths=None, reload_tick=750, reload_ignore_worker_failure=false))]
    fn serve(
        slf: Py<Self>,
        py: Python,
        host: Option<String>,
        port: Option<u16>,
        reload: bool,
        reload_dirs: Option<Vec<String>>,
        reload_ignore_dirs: Option<Vec<String>>,
        reload_ignore_patterns: Option<Vec<String>>,
        reload_ignore_paths: Option<Vec<String>>,
        reload_tick: u64,
        reload_ignore_worker_failure: bool,
    ) -> PyResult<()> {
        if reload && std::env::var_os("FASTRAPI_RELOAD_CHILD").is_none() {
            server::serve_with_reload(
                py,
                reload_dirs,
                reload_ignore_dirs,
                reload_ignore_patterns,
                reload_ignore_paths,
                reload_tick,
                reload_ignore_worker_failure,
            )
        } else {
            server::serve(py, host, port, slf)
        }
    }

    #[pyo3(signature = (router, *, prefix="".to_string(), tags=None))]
    fn include_router(
        &self,
        py: Python<'_>,
        router: Py<PyAPIRouter>,
        prefix: String,
        tags: Option<Py<PyAny>>,
    ) -> PyResult<()> {
        self.router
            .bind(py)
            .borrow()
            .include_router(py, router, prefix, tags)
    }

    #[pyo3(signature = (prefix, router, *, tags=None))]
    fn nest(
        &self,
        py: Python<'_>,
        prefix: String,
        router: Py<PyAPIRouter>,
        tags: Option<Py<PyAny>>,
    ) -> PyResult<()> {
        self.include_router(py, router, prefix, tags)
    }

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
            }
            Ok(func)
        };
        PyCFunction::new_closure(py, Some(c"exception_handler"), None, decorator).map(|f| f.into())
    }

    fn fallback(&self, py: Python, handler: Py<PyAny>) -> PyResult<Py<PyAny>> {
        if let Some(handlers) = &self.exception_handlers {
            handlers.bind(py).set_item(404, handler.clone_ref(py))?;
        }
        Ok(handler)
    }
}

impl FastrAPI {
    #[inline]
    fn _router<'py>(&self, py: Python<'py>) -> pyo3::PyRef<'py, crate::ffi::decorators::PyAPIRouter> {
        self.router.bind(py).borrow()
    }
}
crate::generate_http_methods!(FastrAPI, _router);
