use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyString, PyTuple};
use std::sync::Arc;
use tracing::info;

use super::server;
pub use super::types::FastrAPI;
use crate::globals::MIDDLEWARES;
use crate::http::middleware::{
    parse_cors_params, parse_gzip_params, parse_session_params, parse_trusted_host_params,
    PyMiddleware,
};
use crate::router::PyAPIRouter;
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

        let opts = &kwargs.map(|d| d.clone()).unwrap_or_else(|| PyDict::new(py));
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

    // wish these methods could be abstracted away by macros, but PyO3 doesn't support dynamic method creation or macros in impl blocks, so here we are :(

    #[pyo3(signature = (path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=true, response_model_exclude_unset=false, response_model_exclude_defaults=false, response_model_exclude_none=false, include_in_schema=true, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None))]
    #[allow(unused_variables)]
    fn get<'py>(
        &self,
        py: Python<'py>,
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
    ) -> PyResult<Py<PyAny>> {
        self.router.bind(py).borrow().create_method_decorator(
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
        )
    }

    #[pyo3(signature = (path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=true, response_model_exclude_unset=false, response_model_exclude_defaults=false, response_model_exclude_none=false, include_in_schema=true, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None))]
    #[allow(unused_variables)]
    fn post<'py>(
        &self,
        py: Python<'py>,
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
    ) -> PyResult<Py<PyAny>> {
        self.router.bind(py).borrow().create_method_decorator(
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
        )
    }

    #[pyo3(signature = (path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=true, response_model_exclude_unset=false, response_model_exclude_defaults=false, response_model_exclude_none=false, include_in_schema=true, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None))]
    #[allow(unused_variables)]
    fn put<'py>(
        &self,
        py: Python<'py>,
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
    ) -> PyResult<Py<PyAny>> {
        self.router.bind(py).borrow().create_method_decorator(
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
        )
    }

    #[pyo3(signature = (path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=true, response_model_exclude_unset=false, response_model_exclude_defaults=false, response_model_exclude_none=false, include_in_schema=true, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None))]
    #[allow(unused_variables)]
    fn delete<'py>(
        &self,
        py: Python<'py>,
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
    ) -> PyResult<Py<PyAny>> {
        self.router.bind(py).borrow().create_method_decorator(
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
        )
    }

    #[pyo3(signature = (path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=true, response_model_exclude_unset=false, response_model_exclude_defaults=false, response_model_exclude_none=false, include_in_schema=true, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None))]
    #[allow(unused_variables)]
    fn patch<'py>(
        &self,
        py: Python<'py>,
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
    ) -> PyResult<Py<PyAny>> {
        self.router.bind(py).borrow().create_method_decorator(
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
        )
    }

    #[pyo3(signature = (path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=true, response_model_exclude_unset=false, response_model_exclude_defaults=false, response_model_exclude_none=false, include_in_schema=true, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None))]
    #[allow(unused_variables)]
    fn options<'py>(
        &self,
        py: Python<'py>,
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
    ) -> PyResult<Py<PyAny>> {
        self.router.bind(py).borrow().create_method_decorator(
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
        )
    }

    #[pyo3(signature = (path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=true, response_model_exclude_unset=false, response_model_exclude_defaults=false, response_model_exclude_none=false, include_in_schema=true, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None))]
    #[allow(unused_variables)]
    fn head<'py>(
        &self,
        py: Python<'py>,
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
    ) -> PyResult<Py<PyAny>> {
        self.router.bind(py).borrow().create_method_decorator(
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
        )
    }

    #[pyo3(signature = (path))]
    fn websocket(&self, py: Python<'_>, path: String) -> PyResult<Py<PyAny>> {
        self.router.bind(py).borrow().create_ws_decorator(py, path)
    }

    // decorator for generic Python functions: @app.middleware("smtg")
    fn middleware(&self, py: Python, middleware_type: String) -> PyResult<Py<PyAny>> {
        let decorator = move |args: &Bound<'_, PyTuple>,
                              _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> {
            let py = args.py();
            let func: Py<PyAny> = args.get_item(0)?.unbind(); // 0th item is the function being decorated
            let py_middleware = PyMiddleware::new(func.clone_ref(py));
            let middleware_id = format!("{}_{}", middleware_type, MIDDLEWARES.len());
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

    fn serve(slf: Py<Self>, py: Python, host: Option<String>, port: Option<u16>) -> PyResult<()> {
        server::serve(py, host, port, slf)
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
}
