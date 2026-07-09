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
use tracing::{debug, error};

mod cors;
mod gzip;
mod rate_limit;
mod session;
mod trustedhost;

pub use cors::{CORSMiddleware, build_cors_layer, parse_cors_params};
pub use gzip::{GZipMiddleware, parse_gzip_params};
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
    headers: Vec<(String, String)>,
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
        headers: request
            .headers()
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|val| (k.to_string(), val.to_string())))
            .collect(),
    };

    let result = tokio::task::spawn_blocking(move || {
        Python::attach(|py| {
            let py_dict = PyDict::new(py);
            py_dict.set_item("method", req_info.method).ok();
            py_dict.set_item("path", req_info.path).ok();
            py_dict.set_item("query", req_info.query).ok();

            let headers_dict = PyDict::new(py);
            req_info.headers.into_iter().for_each(|(k, v)| {
                let _ = headers_dict.set_item(k, v);
            });
            py_dict.set_item("headers", headers_dict).ok();

            for middleware in middlewares.iter() {
                let middleware_func = middleware.func.bind(py);
                match middleware_func.as_borrowed().call1((&py_dict,)) {
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

pub async fn logging_middleware(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let start = std::time::Instant::now();
    let response = next.run(request).await;

    debug!(
        "-> {} {} | {} | {:?}",
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
