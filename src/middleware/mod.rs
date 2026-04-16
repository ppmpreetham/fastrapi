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
mod session;
mod trustedhost;

pub use cors::{build_cors_layer, parse_cors_params, CORSMiddleware};
pub use gzip::{parse_gzip_params, GZipMiddleware};
pub use session::{parse_session_params, SessionMiddleware};
pub use trustedhost::{parse_trusted_host_params, TrustedHostMiddleware};

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
            match middleware_func.as_borrowed().call1((py_dict,)) {
                Ok(result) => {
                    if !result.is_none() {
                        return crate::utils::py_to_response(py, &result);
                    }

                    (StatusCode::NO_CONTENT, "CONTINUE").into_response()
                }
                Err(err) => {
                    err.print(py);
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
