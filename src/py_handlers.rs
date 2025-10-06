use pyo3::prelude::*;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

use crate::{utils::{json_to_py_object, py_to_response}, ROUTES};

/// Python handler runners

pub async fn run_py_handler_with_args(
    rt_handle: tokio::runtime::Handle,
    route_key: String,
    payload: serde_json::Value,
) -> Response {
    match rt_handle.spawn_blocking(move || {
        Python::attach(|py| {
            let result = if let Some(py_func) = ROUTES.get(&route_key) {
                let py_payload = json_to_py_object(py, &payload);
                match py_func.call1(py, (py_payload,)) {
                    Ok(result) => Ok(py_to_response(py, &result.into_bound(py))),
                    Err(err) => {
                        err.print(py);
                        Err(())
                    }
                }
            } else {
                eprintln!("Route handler not found for {}", route_key);
                Err(())
            };
            result
        })
    }).await
    {
        Ok(Ok(response)) => response,
        Ok(Err(_)) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn run_py_handler_no_args(
    rt_handle: tokio::runtime::Handle,
    route_key: String,
) -> Response {
    match rt_handle.spawn_blocking(move || {
        Python::attach(|py| {
            let result = if let Some(py_func) = ROUTES.get(&route_key) {
                match py_func.call0(py) {
                    Ok(result) => Ok(py_to_response(py, &result.into_bound(py))),
                    Err(err) => {
                        err.print(py);
                        Err(())
                    }
                }
            } else {
                eprintln!("Route handler not found for {}", route_key);
                Err(())
            };
            result
        })
    }).await
    {
        Ok(Ok(response)) => response,
        Ok(Err(_)) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}