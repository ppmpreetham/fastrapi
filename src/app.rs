use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyTuple};
use std::sync::Arc;
use tracing::info;

use crate::middlewares::{
    parse_cors_params, parse_gzip_params, parse_session_params, parse_trusted_host_params,
    CORSMiddleware, GZipMiddleware, PyMiddleware, SessionMiddleware, TrustedHostMiddleware,
};
use crate::pydantic::parse_route_metadata;
use crate::websocket::websocket as ws_decorator;
use crate::{RouteHandler, MIDDLEWARES, ROUTES};
#[pyclass(name = "FastrAPI")]
pub struct FastrAPI {
    #[pyo3(get, set)]
    pub debug: bool,
    #[pyo3(get, set)]
    pub routes: Option<Py<PyAny>>,
    #[pyo3(get, set)]
    pub title: String,
    #[pyo3(get, set)]
    pub summary: Option<String>,
    #[pyo3(get, set)]
    pub description: String,
    #[pyo3(get, set)]
    pub version: String,
    #[pyo3(get, set)]
    pub openapi_url: String,
    #[pyo3(get, set)]
    pub openapi_tags: Option<Py<PyAny>>,
    #[pyo3(get, set)]
    pub servers: Option<Py<PyAny>>,
    #[pyo3(get, set)]
    pub dependencies: Option<Py<PyAny>>,
    #[pyo3(get, set)]
    pub default_response_class: Py<PyAny>,
    #[pyo3(get, set)]
    pub redirect_slashes: bool,
    #[pyo3(get, set)]
    pub docs_url: Option<String>,
    #[pyo3(get, set)]
    pub redoc_url: Option<String>,
    #[pyo3(get, set)]
    pub swagger_ui_oauth2_redirect_url: Option<String>,
    #[pyo3(get, set)]
    pub swagger_ui_init_oauth: Option<Py<PyAny>>,
    #[pyo3(get, set)]
    pub middleware: Option<Py<PyAny>>,
    #[pyo3(get, set)]
    pub exception_handlers: Option<Py<PyAny>>,
    #[pyo3(get, set)]
    pub on_startup: Option<Py<PyAny>>,
    #[pyo3(get, set)]
    pub on_shutdown: Option<Py<PyAny>>,
    #[pyo3(get, set)]
    pub lifespan: Option<Py<PyAny>>,
    #[pyo3(get, set)]
    pub terms_of_service: Option<String>,
    #[pyo3(get, set)]
    pub contact: Option<Py<PyAny>>,
    #[pyo3(get, set)]
    pub license_info: Option<Py<PyAny>>,
    #[pyo3(get, set)]
    pub openapi_prefix: String,
    #[pyo3(get, set)]
    pub root_path: String,
    #[pyo3(get, set)]
    pub root_path_in_servers: bool,
    #[pyo3(get, set)]
    pub responses: Option<Py<PyAny>>,
    #[pyo3(get, set)]
    pub callbacks: Option<Py<PyAny>>,
    #[pyo3(get, set)]
    pub webhooks: Option<Py<PyAny>>,
    #[pyo3(get, set)]
    pub deprecated: Option<bool>,
    #[pyo3(get, set)]
    pub include_in_schema: bool,
    #[pyo3(get, set)]
    pub swagger_ui_parameters: Option<Py<PyAny>>,
    #[pyo3(get, set)]
    pub generate_unique_id_function: Py<PyAny>,
    #[pyo3(get, set)]
    pub separate_input_output_schemas: bool,
    #[pyo3(get, set)]
    pub openapi_external_docs: Option<Py<PyAny>>,

    // CORS for rust side of things
    pub cors_config: Option<CORSMiddleware>,
    pub trusted_host_config: Option<TrustedHostMiddleware>,
    pub gzip_config: Option<GZipMiddleware>,
    pub session_config: Option<SessionMiddleware>,
}

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
    ) -> PyResult<Self> {
        let default_response_class = if let Some(cls) = default_response_class {
            cls
        } else {
            py.import("fastrapi.responses")?
                .getattr("JSONResponse")?
                .unbind()
        };

        let generate_unique_id_function = if let Some(func) = generate_unique_id_function {
            func
        } else {
            py.eval(c"lambda route: route.__name__", None, None)?
                .unbind()
        };

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
            swagger_ui_oauth2_redirect_url,
            swagger_ui_init_oauth,
            middleware,
            exception_handlers,
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
            cors_config: None,
            trusted_host_config: None,
            gzip_config: None,
            session_config: None,
        })
    }

    #[pyo3(signature = (middleware_class, **kwargs))]
    fn add_middleware(
        &mut self,
        py: Python,
        middleware_class: Py<PyAny>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        let mw_bound = middleware_class.bind(py);
        let cls_name = mw_bound.getattr("__name__")?.extract::<String>()?;

        // empty dict if no kwargs provided
        let empty_dict = PyDict::new(py);
        let opts = kwargs.unwrap_or(&empty_dict);

        match cls_name.as_str() {
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
                // reject unknown classes
                let msg = format!(
                    "Middleware '{}' is not supported. Only CORSMiddleware, TrustedHostMiddleware, GZipMiddleware, and SessionMiddleware are allowed via add_middleware.", 
                    cls_name
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
        }
        Ok(())
    }

    fn get<'py>(&self, path: String, py: Python<'py>) -> PyResult<Py<PyAny>> {
        self.create_decorator("GET", path, py)
    }

    fn post<'py>(&self, path: String, py: Python<'py>) -> PyResult<Py<PyAny>> {
        self.create_decorator("POST", path, py)
    }

    fn put<'py>(&self, path: String, py: Python<'py>) -> PyResult<Py<PyAny>> {
        self.create_decorator("PUT", path, py)
    }

    fn delete<'py>(&self, path: String, py: Python<'py>) -> PyResult<Py<PyAny>> {
        self.create_decorator("DELETE", path, py)
    }

    fn patch<'py>(&self, path: String, py: Python<'py>) -> PyResult<Py<PyAny>> {
        self.create_decorator("PATCH", path, py)
    }

    fn options<'py>(&self, path: String, py: Python<'py>) -> PyResult<Py<PyAny>> {
        self.create_decorator("OPTIONS", path, py)
    }

    fn head<'py>(&self, path: String, py: Python<'py>) -> PyResult<Py<PyAny>> {
        self.create_decorator("HEAD", path, py)
    }

    // decorator for websockets: @app.websocket("/ws")
    fn websocket<'py>(&self, path: String, _py: Python<'py>) -> PyResult<Py<PyAny>> {
        if !path.starts_with('/') {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "WebSocket path must start with '/'",
            ));
        }
        ws_decorator(path)
    }

    // decorator for generic Python functions: @app.middleware("smtg")
    fn middleware(&self, py: Python, middleware_type: String) -> PyResult<Py<PyAny>> {
        let decorator = move |args: &Bound<'_, PyTuple>,
                              _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> {
            let py = args.py();
            let func: Py<PyAny> = args.get_item(0)?.extract()?;
            let py_middleware = PyMiddleware::new(func.clone_ref(py));
            let middleware_id = format!("{}_{}", middleware_type, MIDDLEWARES.len());
            MIDDLEWARES
                .pin()
                .insert(middleware_id.clone(), Arc::new(py_middleware));
            info!("🔗 Registered middleware: {}", middleware_id);
            Ok(func)
        };

        PyCFunction::new_closure(py, None, None, decorator).map(|f| f.into())
    }

    fn serve(&self, py: Python, host: Option<String>, port: Option<u16>) -> PyResult<()> {
        crate::server::serve(py, host, port, self)
    }
}

impl FastrAPI {
    fn create_decorator<'py>(
        &self,
        method: &str,
        path: String,
        py: Python<'py>,
    ) -> PyResult<Py<PyAny>> {
        let route_key = format!("{} {}", method, path);
        let path_for_closure = path.clone();
        let decorator = move |args: &Bound<'_, PyTuple>,
                              _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> {
            let py = args.py();
            let func: Py<PyAny> = args.get_item(0)?.extract()?;
            let func_bound = func.bind(py);
            let (
                param_validators,
                response_type,
                path_param_names,
                query_param_names,
                body_param_names,
                dependencies,
            ) = parse_route_metadata(py, func_bound, &path_for_closure);

            let handler = RouteHandler {
                func: func.clone_ref(py),
                param_validators,
                response_type,
                path_param_names,
                query_param_names,
                body_param_names,
                dependencies,
            };
            ROUTES.pin().insert(route_key.clone(), handler);
            Ok(func)
        };
        PyCFunction::new_closure(py, None, None, decorator).map(|f| f.into())
    }
}
