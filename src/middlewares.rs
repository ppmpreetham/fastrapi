use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use std::sync::Arc;
use tracing::{debug, error};

/// Python middleware wrapper
#[derive(Clone)]
pub struct PyMiddleware {
    pub func: Py<PyAny>,
}
impl PyMiddleware {
    pub fn new(func: Py<PyAny>) -> Self {
        Self { func }
    }
}

/// Convert Axum Request to Python dict representation
fn request_to_py_dict(py: Python<'_>, req: &Request) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);

    // Method
    dict.set_item("method", req.method().as_str())?;

    // URL/Path
    dict.set_item("path", req.uri().path())?;
    dict.set_item("query", req.uri().query().unwrap_or(""))?;

    // Headers
    let headers = PyDict::new(py);

    for (name, value) in req.headers().iter() {
        if let Ok(val_str) = value.to_str() {
            headers.set_item(name.as_str(), val_str)?;
        }
    }

    dict.set_item("headers", headers)?;
    Ok(dict.into())
}

/// Execute Python middleware function

pub async fn execute_py_middleware(
    middleware: Arc<PyMiddleware>,
    request: Request,
    next: Next,
) -> Response {
    // Clone for blocking task
    let middleware = middleware.clone();

    // Store whether to continue or return early
    enum MiddlewareResult {
        Continue(Request),
        EarlyReturn(Response),
    }

    let result = tokio::task::spawn_blocking(move || {
        Python::attach(|py| {
            let py_request = match request_to_py_dict(py, &request) {
                Ok(req) => req,
                Err(e) => {
                    error!("Failed to convert request to Python: {}", e);
                    return MiddlewareResult::EarlyReturn(
                        (StatusCode::INTERNAL_SERVER_ERROR, "Middleware error").into_response(),
                    );
                }
            };

            let middleware_func = middleware.func.bind(py);

            // Call Python middleware with request dict
            match middleware_func.call1((py_request,)) {
                Ok(result) => {
                    // Check if middleware returned a response (blocks request)
                    if !result.is_none() {
                        debug!("Middleware returned early response");
                        // Convert Python response to Axum response
                        return MiddlewareResult::EarlyReturn(crate::utils::py_to_response(
                            py, &result,
                        ));
                    }

                    // Continue to next handler
                    MiddlewareResult::Continue(request)
                }

                Err(e) => {
                    error!("Python middleware error: {}", e);
                    e.print(py);
                    MiddlewareResult::EarlyReturn(
                        (StatusCode::INTERNAL_SERVER_ERROR, "Middleware error").into_response(),
                    )
                }
            }
        })
    })
    .await;

    match result {
        Ok(MiddlewareResult::Continue(request)) => next.run(request).await,
        Ok(MiddlewareResult::EarlyReturn(response)) => response,
        Err(e) => {
            error!("Tokio blocking task error: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Middleware layer for CORS

pub async fn cors_middleware(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
    headers.insert(
        "Access-Control-Allow-Methods",
        "GET, POST, PUT, DELETE, OPTIONS, PATCH".parse().unwrap(),
    );

    headers.insert(
        "Access-Control-Allow-Headers",
        "Content-Type, Authorization".parse().unwrap(),
    );

    response
}

/// Middleware for request logging
pub async fn logging_middleware(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let start = std::time::Instant::now();
    debug!("→ {} {}", method, uri);
    let response = next.run(request).await;
    let duration = start.elapsed();

    debug!(
        "← {} {} - {} ({:?})",
        method,
        uri,
        response.status(),
        duration
    );

    response
}

/// Middleware for adding custom headers
pub async fn header_middleware(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;

    response
        .headers_mut()
        .insert("X-Powered-By", "FastrAPI".parse().unwrap());
    response
}
