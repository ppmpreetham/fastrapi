use pyo3::prelude::*;
use pyo3::types::PyAny;

use crate::http::middleware::{
    CORSMiddleware, GZipMiddleware, SessionMiddleware, TrustedHostMiddleware,
};

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
