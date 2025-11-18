use axum::http::{HeaderName, HeaderValue, Method};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::str::FromStr;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};

#[pyclass(name = "CORSMiddleware")]
#[derive(Clone, Debug)]
pub struct CorsConfig {
    pub allow_origins: Vec<String>,
    pub allow_methods: Vec<String>,
    pub allow_headers: Vec<String>,
    pub allow_credentials: bool,
    pub expose_headers: Vec<String>,
    pub max_age: u64,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allow_origins: vec![],
            allow_methods: vec![
                "GET".to_string(),
                "POST".to_string(),
                "PUT".to_string(),
                "DELETE".to_string(),
            ],
            allow_headers: vec![],
            allow_credentials: false,
            expose_headers: vec![],
            max_age: 600,
        }
    }
}

#[pymethods]
impl CorsConfig {
    #[new]
    #[pyo3(signature = (
        allow_origins=vec![],
        allow_methods=vec!["GET".to_string(), "POST".to_string(), "PUT".to_string(), "DELETE".to_string()],
        allow_headers=vec![],
        allow_credentials=false,
        expose_headers=vec![],
        max_age=600,
    ))]
    fn new(
        _py: Python<'_>,
        allow_origins: Vec<String>,
        allow_methods: Vec<String>,
        allow_headers: Vec<String>,
        allow_credentials: bool,
        expose_headers: Vec<String>,
        max_age: u64,
    ) -> Self {
        Self {
            allow_origins,
            allow_methods,
            allow_headers,
            allow_credentials,
            expose_headers,
            max_age,
        }
    }
}

pub fn build_cors_layer(config: &CorsConfig) -> PyResult<CorsLayer> {
    let mut layer = CorsLayer::new();

    // allow origins
    if config.allow_origins.contains(&"*".to_string()) {
        layer = layer.allow_origin(AllowOrigin::any());
    } else {
        let origins: Vec<HeaderValue> = config
            .allow_origins
            .iter()
            .filter_map(|o| HeaderValue::from_str(o).ok())
            .collect();
        layer = layer.allow_origin(origins);
    }

    // allow methods
    if config.allow_methods.contains(&"*".to_string()) {
        layer = layer.allow_methods(AllowMethods::any());
    } else {
        let methods: Vec<Method> = config
            .allow_methods
            .iter()
            .filter_map(|m| Method::from_str(m).ok())
            .collect();
        layer = layer.allow_methods(methods);
    }

    // allow headers
    if config.allow_headers.contains(&"*".to_string()) {
        layer = layer.allow_headers(AllowHeaders::any());
    } else {
        let headers: Vec<HeaderName> = config
            .allow_headers
            .iter()
            .filter_map(|h| HeaderName::from_str(h).ok())
            .collect();
        layer = layer.allow_headers(headers);
    }

    // allow credentials
    if config.allow_credentials {
        layer = layer.allow_credentials(true);
    }

    // expose headers
    if !config.expose_headers.is_empty() {
        let headers: Vec<HeaderName> = config
            .expose_headers
            .iter()
            .filter_map(|h| HeaderName::from_str(h).ok())
            .collect();
        layer = layer.expose_headers(headers);
    }

    // max age
    layer = layer.max_age(std::time::Duration::from_secs(config.max_age));

    Ok(layer)
}

macro_rules! set_field {
    ($kwargs:expr, $config:expr, $key:expr, $field:ident : $ty:ty) => {
        if let Some(val) = $kwargs.get_item($key)? {
            if !val.is_none() {
                if let Ok(parsed) = val.extract::<$ty>() {
                    $config.$field = parsed;
                }
            }
        }
    };
}

pub fn parse_cors_params(kwargs: &Bound<'_, PyDict>) -> PyResult<CorsConfig> {
    let mut config = CorsConfig::default();

    set_field!(kwargs, config, "allow_origins", allow_origins: Vec<String>);
    set_field!(kwargs, config, "allow_methods", allow_methods: Vec<String>);
    set_field!(kwargs, config, "allow_headers", allow_headers: Vec<String>);
    set_field!(kwargs, config, "allow_credentials", allow_credentials: bool);
    set_field!(kwargs, config, "expose_headers", expose_headers: Vec<String>);
    set_field!(kwargs, config, "max_age", max_age: u64);

    Ok(config)
}

pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();

    // 'fastrapi.middleware'
    let middleware_module = PyModule::new(py, "middleware")?;

    // 'fastrapi.middleware.cors'
    let cors_module = PyModule::new(py, "cors")?;
    cors_module.add_class::<CorsConfig>()?;

    middleware_module.add_submodule(&cors_module)?;
    parent.add_submodule(&middleware_module)?;

    // injection
    let sys_modules = py.import("sys")?.getattr("modules")?;
    sys_modules.set_item("fastrapi.middleware", &middleware_module)?;
    sys_modules.set_item("fastrapi.middleware.cors", &cors_module)?;

    Ok(())
}
