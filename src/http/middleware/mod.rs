use crate::http::responses::convert_auto_response;
use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use std::sync::Arc;
use tracing::error;

pub mod cors;
pub mod gzip;
pub mod httpsredirect;
mod rate_limit;
pub mod registry;
mod session;
mod trustedhost;

use axum::http::HeaderMap;
pub use cors::{CORSMiddleware, build_cors_layer, parse_cors_params};
pub use gzip::{GZipMiddleware, parse_gzip_params};
pub use httpsredirect::{HTTPSRedirectMiddleware, parse_https_redirect_params};
pub use rate_limit::rate_limit;
pub use registry::{
    MIDDLEWARE_REGISTRY, MiddlewareBuilder, MiddlewareContainer, MiddlewareRegistry,
};
pub use session::{SessionMiddleware, parse_session_params};
pub use trustedhost::{TrustedHostMiddleware, parse_trusted_host_params};

#[derive(Clone)]
pub struct PyMiddleware {
    pub func: Py<PyAny>,
}

impl PyMiddleware {
    pub fn new(func: Py<PyAny>) -> Self {
        Self { func }
    }
}

struct PyRequestInfo {
    method: String,
    path: String,
    query: String,
    headers: HeaderMap,
}

enum MiddlewareDecision {
    Continue,
    Respond(Response),
}

pub async fn execute_py_middleware(
    middleware: Arc<PyMiddleware>,
    request: Request,
    next: Next,
) -> Response {
    execute_py_middlewares(Arc::new(vec![middleware]), request, next).await
}

pub async fn execute_py_middlewares(
    middlewares: Arc<Vec<Arc<PyMiddleware>>>,
    request: Request,
    next: Next,
) -> Response {
    let req_info = PyRequestInfo {
        method: request.method().as_str().to_string(),
        path: request.uri().path().to_string(),
        query: request.uri().query().unwrap_or("").to_string(),
        headers: request.headers().clone(),
    };

    let result = tokio::task::spawn_blocking(move || {
        Python::attach(|py| {
            let py_dict = PyDict::new(py);

            let scope_dict = PyDict::new(py);
            scope_dict
                .set_item("type", "http")
                .expect("Failed to set item");
            scope_dict
                .set_item("method", &req_info.method)
                .expect("Failed to set item");
            scope_dict
                .set_item("path", &req_info.path)
                .expect("Failed to set item");
            scope_dict
                .set_item(
                    "query_string",
                    pyo3::types::PyBytes::new(py, req_info.query.as_bytes()),
                )
                .expect("Failed to set item");
            let headers_dict = PyDict::new(py);
            for (k, v) in req_info.headers.iter() {
                headers_dict
                    .set_item(
                        pyo3::types::PyBytes::new(py, k.as_str().as_bytes()),
                        pyo3::types::PyBytes::new(py, v.as_bytes()),
                    )
                    .expect("Failed to set item");
            }
            scope_dict.set_item("headers", &headers_dict).ok();
            py_dict.set_item("scope", scope_dict).ok();
            py_dict.set_item("headers", headers_dict).ok();
            py_dict.set_item("method", &req_info.method).ok();
            py_dict.set_item("path", &req_info.path).ok();

            for middleware in middlewares.iter() {
                let middleware_func = middleware.func.bind(py);
                match middleware_func.call1((&py_dict,)) {
                    Ok(result) => {
                        if !result.is_none() {
                            return MiddlewareDecision::Respond(convert_auto_response(py, &result));
                        }
                    }
                    Err(err) => {
                        err.print(py);
                        return MiddlewareDecision::Respond(
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "Middleware Execution Error",
                            )
                                .into_response(),
                        );
                    }
                }
            }

            MiddlewareDecision::Continue
        })
    })
    .await;

    match result {
        Ok(MiddlewareDecision::Continue) => next.run(request).await,
        Ok(MiddlewareDecision::Respond(response)) => response,
        Err(err) => {
            error!("Tokio task error: {}", err);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
