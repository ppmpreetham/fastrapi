use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use std::str::FromStr;
use std::sync::Arc;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};
use tracing::{debug, error};

/// macro to extract kwargs into config fields safely
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
    // variant for Option types
    ($kwargs:expr, $config:expr, $key:expr, $field:ident : Option<$ty:ty>) => {
        if let Some(val) = $kwargs.get_item($key)? {
            if !val.is_none() {
                if let Ok(parsed) = val.extract::<$ty>() {
                    $config.$field = Some(parsed);
                }
            }
        }
    };
}

// ========================= //
// ==== CORS MIDDLEWARE ==== //
// ========================= //

#[pyclass(name = "CORSMiddleware")]
#[derive(Clone, Debug)]
pub struct CORSMiddleware {
    pub allow_origins: Vec<String>,
    pub allow_methods: Vec<String>,
    pub allow_headers: Vec<String>,
    pub allow_credentials: bool,
    pub expose_headers: Vec<String>,
    pub max_age: u64,
}

impl Default for CORSMiddleware {
    fn default() -> Self {
        Self {
            allow_origins: vec![],
            allow_methods: vec!["GET".into(), "POST".into(), "PUT".into(), "DELETE".into()],
            allow_headers: vec![],
            allow_credentials: false,
            expose_headers: vec![],
            max_age: 600,
        }
    }
}

#[pymethods]
impl CORSMiddleware {
    #[new]
    #[pyo3(signature = (
        allow_origins=vec![],
        allow_methods=vec!["GET".into(), "POST".into(), "PUT".into(), "DELETE".into()],
        allow_headers=vec![],
        allow_credentials=false,
        expose_headers=vec![],
        max_age=600,
    ))]
    fn new(
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

// small parser for app.add_middleware(CORSMiddleware, **kwargs)
pub fn parse_cors_params(kwargs: &Bound<'_, PyDict>) -> PyResult<CORSMiddleware> {
    let mut config = CORSMiddleware::default();
    set_field!(kwargs, config, "allow_origins", allow_origins: Vec<String>);
    set_field!(kwargs, config, "allow_methods", allow_methods: Vec<String>);
    set_field!(kwargs, config, "allow_headers", allow_headers: Vec<String>);
    set_field!(kwargs, config, "allow_credentials", allow_credentials: bool);
    set_field!(kwargs, config, "expose_headers", expose_headers: Vec<String>);
    set_field!(kwargs, config, "max_age", max_age: u64);
    Ok(config)
}

// building of actual Axum Layer happens here
pub fn build_cors_layer(config: &CORSMiddleware) -> PyResult<CorsLayer> {
    let mut layer = CorsLayer::new();

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

    if config.allow_credentials {
        layer = layer.allow_credentials(true);
    }

    if !config.expose_headers.is_empty() {
        let headers: Vec<HeaderName> = config
            .expose_headers
            .iter()
            .filter_map(|h| HeaderName::from_str(h).ok())
            .collect();
        layer = layer.expose_headers(headers);
    }

    layer = layer.max_age(std::time::Duration::from_secs(config.max_age));
    Ok(layer)
}

// ================================= //
// ==== TRUSTED HOST MIDDLEWARE ==== //
// ================================= //

#[pyclass(name = "TrustedHostMiddleware")]
#[derive(Clone, Debug)]
pub struct TrustedHostMiddleware {
    pub allowed_hosts: Vec<String>,
    pub www_redirect: bool,
}

impl Default for TrustedHostMiddleware {
    fn default() -> Self {
        Self {
            allowed_hosts: vec!["*".to_string()],
            www_redirect: true,
        }
    }
}

#[pymethods]
impl TrustedHostMiddleware {
    #[new]
    #[pyo3(signature = (allowed_hosts=None, www_redirect=true))]
    fn new(allowed_hosts: Option<Vec<String>>, www_redirect: bool) -> Self {
        Self {
            allowed_hosts: allowed_hosts.unwrap_or_else(|| vec!["*".to_string()]),
            www_redirect,
        }
    }
}

pub fn parse_trusted_host_params(kwargs: &Bound<'_, PyDict>) -> PyResult<TrustedHostMiddleware> {
    let mut config = TrustedHostMiddleware::default();
    set_field!(kwargs, config, "allowed_hosts", allowed_hosts: Vec<String>);
    set_field!(kwargs, config, "www_redirect", www_redirect: bool);
    Ok(config)
}

// ======================= //
// ====GZIP MIDDLEWARE==== //
// ======================= //

#[pyclass(name = "GZipMiddleware")]
#[derive(Clone, Debug)]
pub struct GZipMiddleware {
    pub minimum_size: u32,
    pub compresslevel: u32,
}

impl Default for GZipMiddleware {
    fn default() -> Self {
        Self {
            minimum_size: 500,
            compresslevel: 9,
        }
    }
}

#[pymethods]
impl GZipMiddleware {
    #[new]
    #[pyo3(signature = (minimum_size=500, compresslevel=9))]
    fn new(minimum_size: u32, compresslevel: u32) -> Self {
        Self {
            minimum_size,
            compresslevel,
        }
    }
}

pub fn parse_gzip_params(kwargs: &Bound<'_, PyDict>) -> PyResult<GZipMiddleware> {
    let mut config = GZipMiddleware::default();
    set_field!(kwargs, config, "minimum_size", minimum_size: u32);
    set_field!(kwargs, config, "compresslevel", compresslevel: u32);
    Ok(config)
}

// ========================== //
// ====SESSION MIDDLEWARE==== //
// ========================== //

#[pyclass(name = "SessionMiddleware")]
#[derive(Clone, Debug)]
pub struct SessionMiddleware {
    pub secret_key: String,
    pub session_cookie: String,
    pub max_age: Option<i64>,
    pub path: String,
    pub same_site: String,
    pub https_only: bool,
    pub domain: Option<String>,
}

// DO NOT TOUCH THIS PART, because there's no default trait for SessionMiddleware (because secret_key is mandatory)

#[pymethods]
impl SessionMiddleware {
    #[new]
    #[pyo3(signature = (
        secret_key,
        session_cookie="session".to_string(),
        max_age=Some(1209600), // 14 days in seconds
        path="/".to_string(),
        same_site="lax".to_string(),
        https_only=false,
        domain=None
    ))]
    fn new(
        secret_key: String,
        session_cookie: String,
        max_age: Option<i64>,
        path: String,
        same_site: String,
        https_only: bool,
        domain: Option<String>,
    ) -> Self {
        Self {
            secret_key,
            session_cookie,
            max_age,
            path,
            same_site,
            https_only,
            domain,
        }
    }
}

pub fn parse_session_params(kwargs: &Bound<'_, PyDict>) -> PyResult<SessionMiddleware> {
    // extract mandatory secret_key
    let secret_key: String = match kwargs.get_item("secret_key")? {
        Some(val) if !val.is_none() => val.extract()?,
        _ => {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "SessionMiddleware requires 'secret_key' argument",
            ))
        }
    };

    let mut config = SessionMiddleware {
        secret_key,
        session_cookie: "session".into(),
        max_age: Some(1209600),
        path: "/".into(),
        same_site: "lax".into(),
        https_only: false,
        domain: None,
    };

    set_field!(kwargs, config, "session_cookie", session_cookie: String);
    set_field!(kwargs, config, "max_age", max_age: Option<i64>);
    set_field!(kwargs, config, "path", path: String);
    set_field!(kwargs, config, "same_site", same_site: String);
    set_field!(kwargs, config, "https_only", https_only: bool);
    set_field!(kwargs, config, "domain", domain: Option<String>);

    Ok(config)
}

// =================================== //
// ====PYTHON DECORATOR MIDDLEWARE==== //
// =================================== //

#[derive(Clone)]
pub struct PyMiddleware {
    pub func: Py<PyAny>,
}
impl PyMiddleware {
    pub fn new(func: Py<PyAny>) -> Self {
        Self { func }
    }
}

// =========================== //
// ====MODULE REGISTRATION==== //
// =========================== //

pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    let middleware_module = PyModule::new(py, "middleware")?;

    middleware_module.add_class::<CORSMiddleware>()?;
    middleware_module.add_class::<TrustedHostMiddleware>()?;
    middleware_module.add_class::<GZipMiddleware>()?;
    middleware_module.add_class::<SessionMiddleware>()?;

    // fastrapi.middleware.cors for backward compatibility
    let cors_module = PyModule::new(py, "cors")?;
    cors_module.add_class::<CORSMiddleware>()?;

    // IF YOU ARE WONDERING WHY WE'RE NOT ADDING CORS_MODULE AS A SUBMODULE HERE, IT'S TO AVOID DOUBLE REGISTRATION ISSUES
    // Instead we just inject it into sys.modules below.
    let sys_modules = py.import("sys")?.getattr("modules")?;

    // 1. fastrapi.middleware
    parent.add_submodule(&middleware_module)?;
    sys_modules.set_item("fastrapi.middleware", &middleware_module)?;

    // 2. fastrapi.middleware.cors (for backward compatibility)
    middleware_module.add_submodule(&cors_module)?;
    sys_modules.set_item("fastrapi.middleware.cors", &cors_module)?;

    Ok(())
}

struct PyRequestInfo {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
}

fn request_to_py_dict(py: Python<'_>, req: &Request) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("method", req.method().as_str())?;
    dict.set_item("path", req.uri().path())?;
    dict.set_item("query", req.uri().query().unwrap_or(""))?;

    let headers = PyDict::new(py);
    for (name, value) in req.headers().iter() {
        if let Ok(val_str) = value.to_str() {
            headers.set_item(name.as_str(), val_str)?;
        }
    }
    dict.set_item("headers", headers)?;
    Ok(dict.into())
}

pub async fn execute_py_middleware(
    middleware: Arc<PyMiddleware>,
    request: Request,
    next: Next,
) -> Response {
    let req_info = PyRequestInfo {
        method: request.method().as_str().to_string(),
        path: request.uri().path().to_string(),
        query: request.uri().query().unwrap_or("").to_string(),
        headers: request
            .headers()
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|val| (k.to_string(), val.to_string())))
            .collect(),
    };

    let middleware = middleware.clone();

    let result = tokio::task::spawn_blocking(move || {
        Python::attach(|py| {
            let py_dict = PyDict::new(py);
            py_dict.set_item("method", req_info.method).ok();
            py_dict.set_item("path", req_info.path).ok();
            py_dict.set_item("query", req_info.query).ok();

            let headers_dict = PyDict::new(py);
            for (k, v) in req_info.headers {
                headers_dict.set_item(k, v).ok();
            }
            py_dict.set_item("headers", headers_dict).ok();

            let middleware_func = middleware.func.bind(py);
            match middleware_func.call1((py_dict,)) {
                Ok(result) => {
                    // program must stop if middleware returns a Response object
                    if !result.is_none() {
                        return crate::utils::py_to_response(py, &result);
                    }
                    // else tell signal to continue
                    (StatusCode::NO_CONTENT, "CONTINUE").into_response()
                }
                Err(e) => {
                    e.print(py);
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Middleware Execution Error",
                    )
                        .into_response()
                }
            }
        })
    })
    .await;

    match result {
        Ok(response) => {
            if response.status() == StatusCode::NO_CONTENT {
                next.run(request).await
            } else {
                response
            }
        }
        Err(e) => {
            error!("Tokio task error: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// TODO: to make these dummy middlewares real in the future
pub async fn logging_middleware(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let start = std::time::Instant::now();
    let response = next.run(request).await;
    debug!(
        "→ {} {} | {} | {:?}",
        method,
        uri,
        response.status(),
        start.elapsed()
    );
    response
}

pub async fn header_middleware(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert("X-Powered-By", "FastrAPI".parse().unwrap());
    response
}
