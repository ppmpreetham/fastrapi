use axum::{
    extract::{ConnectInfo, Extension},
    response::Html,
    routing::{
        delete as axum_delete, get as axum_get, head as axum_head, options as axum_options,
        patch as axum_patch, post as axum_post, put as axum_put,
    },
    Json, Router,
};
use dashmap::DashMap;
use once_cell::sync::Lazy;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule, PyTuple};
use smallvec::SmallVec;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{debug, error, info, warn, Level};
use tracing_subscriber;

mod openapi;
mod py_handlers;
mod pydantic;
mod status;
mod utils;

use crate::py_handlers::{run_py_handler_no_args, run_py_handler_with_args};
use crate::pydantic::{is_pydantic_model, register_pydantic_integration};
use crate::status::create_status_submodule;
use openapi::build_openapi_spec;

const SWAGGER_HTML: &str = include_str!("../static/swagger-ui.html");

// response type tracking
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseType {
    Json,
    Html,
    PlainText,
    Redirect,
    Auto, // for untyped responses
}

//  pydantic v1 or v2
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidationMethod {
    V1,
    V2,
}

#[derive(Clone)]
pub struct RouteHandler {
    pub func: Py<PyAny>,
    pub param_validators: Vec<(String, Py<PyAny>, ValidationMethod)>,
    pub response_type: ResponseType,
}

pub static ROUTES: Lazy<DashMap<String, RouteHandler>> = Lazy::new(DashMap::new);

static PYTHON_RUNTIME: Lazy<tokio::runtime::Runtime> = Lazy::new(|| {
    let worker_threads = num_cpus::get().max(4).min(16);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .thread_name("python-handler")
        .enable_all()
        .build()
        .expect("Failed to create Python runtime")
});

#[derive(Clone)]
struct AppState {
    rt_handle: tokio::runtime::Handle,
}

#[pyclass]
pub struct FastrAPI {
    #[allow(dead_code)]
    router: Arc<DashMap<String, RouteHandler>>,
}

use smartstring::alias::String as SmartString;

// detects response type
fn get_response_type(py: Python, func: &Bound<PyAny>) -> ResponseType {
    if let Ok(annotations) = func.getattr("__annotations__") {
        if let Ok(dict) = annotations.cast::<PyDict>() {
            if let Ok(Some(return_type)) = dict.get_item("return") {
                let return_str = format!("{:?}", return_type);
                if return_str.contains("HTMLResponse") {
                    return ResponseType::Html;
                }
                if return_str.contains("JSONResponse") {
                    return ResponseType::Json;
                }
                if return_str.contains("PlainTextResponse") {
                    return ResponseType::PlainText;
                }
                if return_str.contains("RedirectResponse") {
                    return ResponseType::Redirect;
                }
            }
        }
    }
    ResponseType::Auto
}

// helper function to parse annotations once (also returns response type)
fn parse_route_metadata(
    py: Python,
    func: &Bound<PyAny>,
) -> (Vec<(String, Py<PyAny>, ValidationMethod)>, ResponseType) {
    let mut validators = Vec::new();
    let response_type = get_response_type(py, func);

    if let Ok(annotations) = func.getattr("__annotations__") {
        if let Ok(ann_dict) = annotations.cast::<PyDict>() {
            for (key, value) in ann_dict.iter() {
                if let Ok(param_name) = key.extract::<String>() {
                    if param_name != "return" && is_pydantic_model(py, &value) {
                        // cache validation method
                        let method = if value.hasattr("model_validate").unwrap_or(false) {
                            ValidationMethod::V2
                        } else {
                            ValidationMethod::V1
                        };

                        validators.push((param_name, value.unbind(), method)); // store it
                    }
                }
            }
        }
    }
    (validators, response_type)
}

#[pymethods]
impl FastrAPI {
    #[new]
    fn new() -> Self {
        FastrAPI {
            router: Arc::new(DashMap::new()),
        }
    }

    fn register_route(&self, path: String, func: Py<PyAny>, method: Option<String>) {
        Python::attach(|py| {
            let func_bound = func.bind(py);
            let (param_validators, response_type) = parse_route_metadata(py, func_bound);

            let handler = RouteHandler {
                func: func.clone_ref(py),
                param_validators: param_validators.clone(),
                response_type,
            };

            let method = method.unwrap_or_else(|| "GET".to_string()).to_uppercase();
            let mut key = SmartString::new();
            key.push_str(&method);
            key.push(' ');
            key.push_str(&path);

            ROUTES.insert((&key).to_string(), handler);
            info!(
                "✅ Registered route [{}] (validators: {}, type: {:?})",
                key,
                param_validators.len(),
                response_type
            );
        });
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

    fn create_decorator<'py>(
        &self,
        method: &str,
        path: String,
        py: Python<'py>,
    ) -> PyResult<Py<PyAny>> {
        let route_key = format!("{} {}", method, path);

        let decorator = move |args: &Bound<'_, PyTuple>,
                              _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> {
            let py = args.py();
            let func: Py<PyAny> = args.get_item(0)?.extract()?;
            let func_bound = func.bind(py);

            let (param_validators, response_type) = parse_route_metadata(py, func_bound);

            let handler = RouteHandler {
                func: func.clone_ref(py),
                param_validators: param_validators.clone(),
                response_type,
            };

            ROUTES.insert(route_key.clone(), handler);
            info!(
                "🧩 Added decorated route [{}] (validators: {}, type: {:?})",
                route_key,
                param_validators.len(),
                response_type
            );

            Ok(func)
        };

        PyCFunction::new_closure(py, None, None, decorator).map(|f| f.into())
    }

    fn serve(&self, py: Python, host: Option<String>, port: Option<u16>) -> PyResult<()> {
        tracing_subscriber::fmt()
            .with_max_level(Level::DEBUG)
            .with_target(false)
            .init();

        info!("🚀 Starting FastrAPI...");

        let host = host.unwrap_or_else(|| "127.0.0.1".to_string());
        let port = port.unwrap_or(8000);

        let rt_handle = PYTHON_RUNTIME.handle().clone();
        let app_state = AppState { rt_handle };

        let mut app = Router::new();

        println!("🧩 Registered routes:");
        for key in ROUTES.iter() {
            println!("   • {}", key.key());
        }

        let openapi_spec = build_openapi_spec(py, &ROUTES);
        let openapi_json = Arc::new(
            serde_json::to_value(&openapi_spec).expect("Failed to serialize OpenAPI spec"),
        );
        info!(
            "✅ OpenAPI spec generated with {} paths",
            openapi_spec.paths.len()
        );

        for entry in ROUTES.iter() {
            let route_key: Arc<str> = entry.key().clone().into();
            let parts: SmallVec<[&str; 2]> = route_key.splitn(2, ' ').collect();

            if parts.len() != 2 {
                warn!("⚠️ Invalid route key format: {}", route_key);
                continue;
            }

            let method = parts[0];
            let path = parts[1].to_string();

            debug!("🔧 Building route: [{} {}]", method, path);

            match method {
                "GET" | "HEAD" | "OPTIONS" => {
                    let route_key = Arc::clone(&route_key);
                    let handler_fn =
                        move |Extension(state): Extension<AppState>,
                              ConnectInfo(_addr): ConnectInfo<SocketAddr>| {
                            let route_key = Arc::clone(&route_key);
                            async move {
                                #[cfg(feature = "verbose-logging")]
                                debug!("{} from {}", route_key, _addr);
                                run_py_handler_no_args(state.rt_handle, route_key).await
                            }
                        };

                    app = match method {
                        "GET" => app.route(&path, axum_get(handler_fn)),
                        "HEAD" => app.route(&path, axum_head(handler_fn)),
                        "OPTIONS" => app.route(&path, axum_options(handler_fn)),
                        _ => app,
                    };
                }
                "POST" | "PUT" | "DELETE" | "PATCH" => {
                    let route_key = Arc::clone(&route_key);
                    let handler_fn =
                        move |Extension(state): Extension<AppState>,
                              ConnectInfo(_addr): ConnectInfo<SocketAddr>,
                              Json(payload): Json<serde_json::Value>| {
                            let route_key = Arc::clone(&route_key);
                            async move {
                                #[cfg(feature = "verbose-logging")]
                                debug!("📥 {} from {}", route_key, _addr);
                                run_py_handler_with_args(state.rt_handle, route_key, payload).await
                            }
                        };

                    app = match method {
                        "POST" => app.route(&path, axum_post(handler_fn)),
                        "PUT" => app.route(&path, axum_put(handler_fn)),
                        "DELETE" => app.route(&path, axum_delete(handler_fn)),
                        "PATCH" => app.route(&path, axum_patch(handler_fn)),
                        _ => app,
                    };
                }
                _ => warn!("⚠️ Ignoring unknown HTTP method: {}", method),
            }
        }

        app = app.route(
            "/api-docs/openapi.json",
            axum_get(move || {
                let json = openapi_json.clone();
                async move { Json(json.as_ref().clone()) }
            }),
        );
        app = app.route("/docs", axum_get(|| async { Html(SWAGGER_HTML) }));

        app = app.layer(axum::Extension(app_state));

        py.detach(move || {
            PYTHON_RUNTIME.block_on(async move {
                let addr = format!("{}:{}", host, port);

                let listener = match TcpListener::bind(&addr).await {
                    Ok(l) => l,
                    Err(e) => {
                        error!("Failed to bind to {}: {}", addr, e);
                        return;
                    }
                };

                info!("🚀 FastrAPI running at http://{}", addr);
                info!("📚 Swagger UI available at http://{}/docs", addr);
                info!("📄 OpenAPI spec at http://{}/api-docs/openapi.json", addr);

                if let Err(e) = axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
                {
                    error!("Server error: {}", e);
                }
            });
        });

        Ok(())
    }
}

#[pyfunction]
fn get_decorator(func: Py<PyAny>, path: String) -> PyResult<()> {
    Python::attach(|py| {
        let func_bound = func.bind(py);
        let (param_validators, response_type) = parse_route_metadata(py, func_bound);

        let handler = RouteHandler {
            func: func.clone_ref(py),
            param_validators: param_validators.clone(),
            response_type,
        };

        let key = format!("GET {}", path);
        ROUTES.insert(key.clone(), handler);
        info!(
            "🔗 Registered via get_decorator [{}] (validators: {}, type: {:?})",
            key,
            param_validators.len(),
            response_type
        );
    });
    Ok(())
}

// simple python wrapper classes, just to hold data
#[pyclass(name = "HTMLResponse")]
#[derive(Clone)]
pub struct PyHTMLResponse {
    #[pyo3(get)]
    pub content: String,
    #[pyo3(get)]
    pub status_code: u16,
}

#[pymethods]
impl PyHTMLResponse {
    #[new]
    #[pyo3(signature = (content, status_code=200))]
    fn new(content: String, status_code: u16) -> Self {
        Self {
            content,
            status_code,
        }
    }
}

#[pyclass(name = "JSONResponse")]
#[derive(Clone)]
pub struct PyJSONResponse {
    #[pyo3(get)]
    pub content: Py<PyAny>,
    #[pyo3(get)]
    pub status_code: u16,
}

#[pymethods]
impl PyJSONResponse {
    #[new]
    #[pyo3(signature = (content, status_code=200))]
    fn new(content: Py<PyAny>, status_code: u16) -> Self {
        Self {
            content,
            status_code,
        }
    }
}

#[pyclass(name = "PlainTextResponse")]
#[derive(Clone)]
pub struct PyPlainTextResponse {
    #[pyo3(get)]
    pub content: String,
    #[pyo3(get)]
    pub status_code: u16,
}

#[pymethods]
impl PyPlainTextResponse {
    #[new]
    #[pyo3(signature = (content, status_code=200))]
    fn new(content: String, status_code: u16) -> Self {
        Self {
            content,
            status_code,
        }
    }
}

#[pyclass(name = "RedirectResponse")]
#[derive(Clone)]
pub struct PyRedirectResponse {
    #[pyo3(get)]
    pub url: String,
    #[pyo3(get)]
    pub status_code: u16,
}

#[pymethods]
impl PyRedirectResponse {
    #[new]
    #[pyo3(signature = (url, status_code=307))]
    fn new(url: String, status_code: u16) -> Self {
        Self { url, status_code }
    }
}

fn create_responses_submodule(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    let responses_module = PyModule::new(py, "responses")?;

    responses_module.add_class::<PyJSONResponse>()?;
    responses_module.add_class::<PyHTMLResponse>()?;
    responses_module.add_class::<PyPlainTextResponse>()?;
    responses_module.add_class::<PyRedirectResponse>()?;

    parent.add_submodule(&responses_module)?;

    // registering in sys.modules for import support (DONT TOUCH THIS)
    py.import("sys")?
        .getattr("modules")?
        .set_item("fastrapi.responses", &responses_module)?;

    Ok(())
}

#[pymodule(gil_used = false)]
fn fastrapi(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<FastrAPI>()?;
    m.add_function(wrap_pyfunction!(get_decorator, m)?)?;

    create_responses_submodule(m)?;
    create_status_submodule(m)?;
    register_pydantic_integration(m)?;

    Ok(())
}
