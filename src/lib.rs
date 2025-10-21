use axum::{
    extract::{ConnectInfo, Extension},
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
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{debug, error, info, warn, Level};
use tracing_subscriber;
use utoipa::OpenApi as ApiDoc;
use utoipa_swagger_ui::SwaggerUi;

mod py_handlers;
mod pydantic;
mod utils;

use crate::py_handlers::{run_py_handler_no_args, run_py_handler_with_args};
use crate::pydantic::register_pydantic_integration;

pub static ROUTES: Lazy<DashMap<String, Py<PyAny>>> = Lazy::new(DashMap::new);

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

#[derive(ApiDoc)]
#[openapi(paths(), components(), tags())]
struct ApiDocumentation;

#[pyclass]
pub struct FastrAPI {
    #[allow(dead_code)]
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

            ROUTES.insert(route_key.clone(), func.clone_ref(py));
            info!("🧩 Added decorated route [{}]", route_key);

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

        // Use Arc<str> to avoid cloning on every request
        for entry in ROUTES.iter() {
            let route_key: Arc<str> = entry.key().clone().into();
            let parts: Vec<&str> = route_key.splitn(2, ' ').collect();

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
                              ConnectInfo(addr): ConnectInfo<SocketAddr>| {
                            let route_key = Arc::clone(&route_key);
                            async move {
                                debug!("📥 {} from {}", route_key, addr);
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
                              ConnectInfo(addr): ConnectInfo<SocketAddr>,
                              Json(payload): Json<serde_json::Value>| {
                            let route_key = Arc::clone(&route_key);
                            async move {
                                debug!("📥 {} from {}", route_key, addr);
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

        app = app.layer(axum::Extension(app_state));
        app = app.merge(
            SwaggerUi::new("/docs").url("/api-docs/openapi.json", ApiDocumentation::openapi()),
        );

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
    let key = format!("GET {}", path);
    ROUTES.insert(key.clone(), func);
    info!("🔗 Registered via get_decorator [{}]", key);
    Ok(())
}

#[pymodule(gil_used = false)]
fn fastrapi(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<FastrAPI>()?;
    m.add_function(wrap_pyfunction!(get_decorator, m)?)?;
    register_pydantic_integration(m)?;
    Ok(())
}
