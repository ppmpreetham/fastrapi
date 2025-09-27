use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule, PyTuple};
use axum::{
    http::StatusCode,
    response::IntoResponse,
    routing::{get as axum_get, post as axum_post},
    Router,
    Json,
};
use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::sync::Arc;
use tokio::net::TcpListener;
use serde_json::{Value, Map};

static ROUTES: Lazy<DashMap<String, Py<PyAny>>> = Lazy::new(|| DashMap::new());

#[pyclass]
pub struct FastrAPI {
    router: Arc<DashMap<String, Py<PyAny>>>, // reference to routes
}

#[pymethods]
impl FastrAPI {
    #[new]
    fn new() -> Self {
        FastrAPI {
            router: Arc::new(DashMap::new()),
        }
    }

    /// registering a python callback for a route
    fn register_route(&self, path: String, func: Py<PyAny>, method: Option<String>) {
        let method = method.unwrap_or_else(|| "GET".to_string()).to_uppercase();
        let key = format!("{} {}", method, path);
        ROUTES.insert(key, func);
    }

    /// helper fn for GET
    #[staticmethod]
    fn get_decorator(func: Py<PyAny>, path: String) -> PyResult<()> {
        let key = format!("GET {}", path);
        ROUTES.insert(key, func);
        Ok(())
    }

    /// @app.get(<path>)
    fn get<'py>(&self, path: String, py: Python<'py>) -> PyResult<Py<PyAny>> {
        let path_clone = path.clone();

        let decorator = move |args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>| -> PyResult<Py<PyAny>> {
            Python::attach(|py| {                      // acquire GIL here
                let func: Py<PyAny> = args.get_item(0)?.extract()?;
                ROUTES.insert(format!("GET {}", path_clone), func.clone_ref(py));
                Ok(func.into())
            })
        };

        PyCFunction::new_closure(py, None, None, decorator).map(|f| f.into())
    }


    /// @app.post(...)
    fn post<'py>(&self, path: String, py: Python<'py>) -> PyResult<Py<PyAny>> {
    let path_clone = path.clone();

        let decorator = move |args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>| -> PyResult<Py<PyAny>> {
            Python::attach(|py| {
                let func: Py<PyAny> = args.get_item(0)?.extract()?;
                ROUTES.insert(format!("POST {}", path_clone), func.clone_ref(py));
                Ok(func.into())
            })
        };

        PyCFunction::new_closure(py, None, None, decorator).map(|f| f.into())
    }


    /// axum route serving
    fn serve(&self, py: Python, host: Option<String>, port: Option<u16>) -> PyResult<()> {
        let host = host.unwrap_or_else(|| "127.0.0.1".to_string());
        let port = port.unwrap_or(8000);

        // tokio runtime
        let rt = tokio::runtime::Runtime::new()?;
        let mut app = Router::new();

        for entry in ROUTES.iter() {
            let parts: Vec<&str> = entry.key().splitn(2, ' ').collect();
            let method = parts[0];
            let path = parts[1].to_string();
            
            let route_key = entry.key().clone();
            let rt_handle = rt.handle().clone();

            let handler = move || async move {
                let route_key = route_key.clone();
                match rt_handle.spawn_blocking(move || {
                    Python::attach(|py| {
                        if let Some(py_func) = ROUTES.get(&route_key) {
                            let result = py_func.call0(py);
                            result.map(|result| py_to_response(&result.into_bound(py)))
                        } else {
                            Err(pyo3::PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                                format!("Route handler not found for {}", route_key)))
                        }
                    })
                }).await {
                    Ok(Ok(response)) => response.into_response(),
                    Ok(Err(err)) => {
                        Python::attach(|py| err.print(py));
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    }
                    Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                }
            };

            if method == "GET" {
                app = app.route(&path, axum_get(handler));
            } else if method == "POST" {
                app = app.route(&path, axum_post(handler));
            }
        }

        py.detach(move || {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let addr = format!("{}:{}", host, port);
                    let listener = TcpListener::bind(&addr).await.unwrap();
                    println!("🚀 FastrAPI running at http://{}", addr);
                    axum::serve(listener, app).await.unwrap();
                });
        });

        Ok(())
    }
}

fn py_dict_to_json(dict: &Bound<'_, PyDict>) -> Value {
    let mut map = Map::new();

    for (key, value) in dict.iter() {
        let k: String = match key.extract() {
            Ok(s) => s,
            Err(_) => continue, // skip non-string keys
        };

        if let Ok(s) = value.extract::<String>() {
            map.insert(k, Value::String(s));
        } else if let Ok(i) = value.extract::<i64>() {
            map.insert(k, Value::Number(i.into()));
        } else if let Ok(f) = value.extract::<f64>() {
            if let Some(num) = serde_json::Number::from_f64(f) {
                map.insert(k, Value::Number(num));
            } else {
                map.insert(k, Value::Null);
            }
        } else if let Ok(b) = value.extract::<bool>() {
            map.insert(k, Value::Bool(b));
        } else if value.is_none() {
            map.insert(k, Value::Null);
        } else if let Ok(nested) = value.downcast::<PyDict>() {
            map.insert(k, py_dict_to_json(&nested));
        } else {
            map.insert(k, Value::String(format!("{:?}", value)));
        }
    }

    Value::Object(map)
}

/// py values to axum responses
fn py_to_response(obj: &Bound<'_, PyAny>) -> impl IntoResponse {
    if let Ok(s) = obj.extract::<String>() {
        s.into_response()
    } else if let Ok(i) = obj.extract::<i64>() {
        i.to_string().into_response()
    } else if let Ok(dict) = obj.downcast::<PyDict>() {
        // Convert dict to JSON
        let json = py_dict_to_json(dict);
        Json(json).into_response()
    } else if obj.is_none() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        format!("{:?}", obj).into_response()
    }
}

/// same helper for GET decorator
#[pyfunction]
fn get_decorator(func: Py<PyAny>, path: String) -> PyResult<()> {
    let key = format!("GET {}", path);
    ROUTES.insert(key, func);
    Ok(())
}

#[pymodule]
fn fastrapi(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<FastrAPI>()?;
    m.add_function(wrap_pyfunction!(get_decorator, m)?)?;
    Ok(())
}
