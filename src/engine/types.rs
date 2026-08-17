use pyo3::prelude::*;
use pyo3::types::PyAny;
use smart_default::SmartDefault;

use crate::{decorators::PyAPIRouter, http::middleware::MiddlewareContainer};

/// sub-FastrAPI` app (`app.mount("/api", sub_api)`).
#[derive(Clone)]
pub struct SubAppMount {
    pub path: String,
    pub app: Py<FastrAPI>,
}

#[pyclass(frozen, new = "from_fields", get_all, from_py_object, eq)]
#[derive(SmartDefault, Clone, Debug, PartialEq, Eq)]
pub struct StaticMount {
    pub path: String,
    pub directory: String,
    #[default(false)]
    pub html: bool,
    #[default(false)]
    pub follow_symlink: bool,
    #[default(None)]
    pub name: Option<String>,
}

#[pyclass(frozen, new = "from_fields", get_all, from_py_object, eq)]
#[derive(SmartDefault, Clone, Debug, PartialEq, Eq)]
pub struct FrontendMount {
    pub path: String,
    pub directory: String,
    #[default(Some("auto".to_string()))]
    pub fallback: Option<String>,
    #[default(true)]
    pub check_dir: bool,
}

#[pyclass(frozen, new = "from_fields", get_all, from_py_object, eq)]
#[derive(SmartDefault, Clone, Debug, PartialEq, Eq)]
pub struct PrometheusConfig {
    #[default("/metrics".to_string())]
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
    pub dependency_overrides: Option<Py<PyAny>>,
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
    #[pyo3(get, set)]
    pub router: Py<PyAPIRouter>,

    pub(crate) static_mounts: Vec<StaticMount>,
    pub(crate) frontend_mounts: Vec<FrontendMount>,
    pub(crate) app_mounts: Vec<SubAppMount>,
    pub(crate) prometheus_config: Option<PrometheusConfig>,
    pub(crate) middlewares: MiddlewareContainer,
}
