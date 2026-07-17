use pyo3::prelude::*;
use pyo3::types::PyAny;

use crate::{
    decorators::PyAPIRouter,
    http::middleware::{
        CORSMiddleware, GZipMiddleware, HTTPSRedirectMiddleware, SessionMiddleware,
        TrustedHostMiddleware,
    },
};

#[derive(Clone)]
pub struct StaticMount {
    pub path: String,
    pub directory: String,
    pub html: bool,
    pub follow_symlink: bool,
    pub name: Option<String>,
}

#[derive(Clone)]
pub struct FrontendMount {
    pub path: String,
    pub directory: String,
    pub fallback: Option<String>,
    pub check_dir: bool,
}

#[derive(Clone)]
pub struct PrometheusConfig {
    pub metrics_path: String,
}

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
    pub scalar_url: Option<String>,
    #[pyo3(get, set)]
    pub elements_url: Option<String>,
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
    #[pyo3(get, set)]
    pub sync_to_threadpool: bool,
    #[pyo3(get, set)]
    pub max_body_size: Option<usize>,
    #[pyo3(get, set)]
    pub max_field_size: Option<usize>,
    #[pyo3(get, set)]
    pub max_file_size: Option<usize>,
    #[pyo3(get, set)]
    pub reject_unknown_multipart_fields: bool,
    #[pyo3(get, set)]
    pub trace_requests: bool,
    #[pyo3(get, set)]
    pub catch_panics: bool,
    #[pyo3(get, set)]
    pub request_timeout: Option<u64>,
    #[pyo3(get, set)]
    pub request_id_header: Option<String>,
    #[pyo3(get, set)]
    pub powered_by_header: Option<String>,
    pub static_mounts: Vec<StaticMount>,
    pub frontend_mounts: Vec<FrontendMount>,
    pub prometheus_config: Option<PrometheusConfig>,

    // CORS for rust side of things
    pub cors_config: Option<CORSMiddleware>,
    pub trusted_host_config: Option<TrustedHostMiddleware>,
    pub https_redirect_config: Option<HTTPSRedirectMiddleware>,
    pub gzip_config: Option<GZipMiddleware>,
    pub session_config: Option<SessionMiddleware>,

    #[pyo3(get)]
    pub router: Py<PyAPIRouter>,
}
