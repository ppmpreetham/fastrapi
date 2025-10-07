use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule, PyTuple};
use axum::{
    routing::{get as axum_get, post as axum_post, put as axum_put, delete as axum_delete, patch as axum_patch, options as axum_options, head as axum_head},
    Router,
    Json,
    extract::{Extension, ConnectInfo},
};
use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::sync::Arc;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing::{info, debug, warn, Level};
use tracing_subscriber;

mod utils;
mod py_handlers;
mod pydantic;

use crate::py_handlers::{run_py_handler_no_args, run_py_handler_with_args};
// use crate::utils::shutdown_signal;
use crate::pydantic::register_pydantic_integration;

pub static ROUTES: Lazy<DashMap<String, Py<PyAny>>> = Lazy::new(|| DashMap::new());

#[derive(Clone)]
struct AppState {
    rt_handle: tokio::runtime::Handle,
}

/// FastrAPI class
#[pyclass]
pub struct FastrAPI {
    router: Arc<DashMap<String, Py<PyAny>>>,
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
        let method = method.unwrap_or_else(|| "GET".to_string()).to_uppercase();
        let key = format!("{} {}", method, path);
        ROUTES.insert(key.clone(), func);
        info!("✅ Registered route [{}]", key);
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

    fn create_decorator<'py>(&self, method: &str, path: String, py: Python<'py>) -> PyResult<Py<PyAny>> {
        let route_key = format!("{} {}", method, path);

        let decorator = move |args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>| -> PyResult<Py<PyAny>> {
            let py = args.py();
            let func: Py<PyAny> = args.get_item(0)?.extract()?;
            ROUTES.insert(route_key.clone(), func.clone_ref(py));
            info!("🧩 Added decorated route [{}]", route_key);
            Ok(func.into())
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

        let rt = tokio::runtime::Runtime::new()?;
        let rt_handle = rt.handle().clone();
        let app_state = AppState { rt_handle: rt_handle.clone() };

        let mut app = Router::new();

        println!("🧩 Registered routes:");
        for key in ROUTES.iter() {
            println!("   • {}", key.key());
        }

        for entry in ROUTES.iter() {
            // Get the full route key as a String (owned)
            let route_key = entry.key().to_string();
            
            // Split the route key into method and path
            let parts: Vec<String> = route_key.splitn(2, ' ')
                .map(|s| s.to_string())
                .collect();
            
            // Make sure we have both method and path parts
            if parts.len() != 2 {
                warn!("⚠️ Invalid route key format: {}", route_key);
                continue;
            }
            
            // Extract method and path as owned Strings
            let method = parts[0].clone();
            let path = parts[1].clone();

            debug!("🔧 Building route: [{} {}]", method, path);

            match method.as_str() {
                "GET" | "HEAD" | "OPTIONS" => {
                    let route_key_clone = route_key.clone();
                    let method_clone = method.clone();
                    let handler_fn = move |Extension(state): Extension<AppState>, ConnectInfo(addr): ConnectInfo<SocketAddr>| {
                        let route_key = route_key_clone.clone();
                        let method = method_clone.clone();
                        async move {
                            println!("📥 Incoming {} request to {}", method, route_key);
                            println!("Client IP: {}", addr);
                            run_py_handler_no_args(state.rt_handle, route_key).await
                        }
                    };

                    app = match method.as_str() {
                        "GET" => app.route(&path, axum_get(handler_fn)),
                        "HEAD" => app.route(&path, axum_head(handler_fn)),
                        "OPTIONS" => app.route(&path, axum_options(handler_fn)),
                        _ => app,
                    };
                }
                "POST" | "PUT" | "DELETE" | "PATCH" => {
                    let route_key_clone = route_key.clone();
                    let method_clone = method.clone();
                    let handler_fn = move |Extension(state): Extension<AppState>, ConnectInfo(addr): ConnectInfo<SocketAddr>, Json(payload): Json<serde_json::Value>| {
                        let route_key = route_key_clone.clone();
                        let method = method_clone.clone();
                        async move {
                            println!("📥 Incoming {} request to {}", method, route_key);
                            println!("Client IP: {}", addr);
                            println!("Payload: {}", payload);
                            run_py_handler_with_args(state.rt_handle, route_key, payload).await
                        }
                    };

                    app = match method.as_str() {
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

        app = app.layer(axum::Extension(app_state));

        py.detach(move || {
            rt.block_on(async move {
                let addr = format!("{}:{}", host, port);
                let listener = TcpListener::bind(&addr).await.unwrap();
                info!("🚀 FastrAPI running at http://{}", addr);
                axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
                    // .with_graceful_shutdown(shutdown_signal())
                    .await
                    .unwrap();
            });
        });

        Ok(())
    }
}

// Python module
#[pyfunction]
fn get_decorator(func: Py<PyAny>, path: String) -> PyResult<()> {
    let key = format!("GET {}", path);
    ROUTES.insert(key.clone(), func);
    info!("🔗 Registered via get_decorator [{}]", key);
    Ok(())
}

#[pymodule]
fn fastrapi(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<FastrAPI>()?;
    m.add_function(wrap_pyfunction!(get_decorator, m)?)?;
    register_pydantic_integration(m)?;
    Ok(())
}