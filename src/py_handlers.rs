use crate::pydantic::validate_with_pydantic;
use crate::{
    utils::{json_to_py_object, py_to_response},
    ROUTES,
};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use pyo3::prelude::*;
use std::sync::Arc;
use tracing::error;

/// For routes WITH payload (POST, PUT, PATCH, DELETE)
pub async fn run_py_handler_with_args(
    rt_handle: tokio::runtime::Handle,
    route_key: Arc<str>,
    payload: serde_json::Value,
) -> Response {
    match rt_handle
        .spawn_blocking(move || {
            Python::attach(|py| {
                let entry = match ROUTES.get(route_key.as_ref()) {
                    Some(e) => e,
                    None => {
                        error!("Route handler not found: {}", route_key);
                        return (StatusCode::NOT_FOUND, "Route handler not found").into_response();
                    }
                };

                let handler = entry.value();
                let py_func = handler.func.bind(py);

                let py_payload = if handler.needs_validation {
                    if let Some(ref validator) = handler.validator_class {
                        match validate_with_pydantic(py, &validator.bind(py), &payload) {
                            Ok(validated) => validated,
                            Err(err_resp) => return err_resp,
                        }
                    } else {
                        json_to_py_object(py, &payload)
                    }
                } else {
                    json_to_py_object(py, &payload)
                };

                match py_func.call1((py_payload,)) {
                    Ok(result) => py_to_response(py, &result),
                    Err(err) => {
                        err.print(py);
                        error!("Error in route handler {}: {}", route_key, err);
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Error in route handler: {}", err),
                        )
                            .into_response()
                    }
                }
            })
        })
        .await
    {
        Ok(response) => response,
        Err(e) => {
            error!("Tokio spawn_blocking error: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// For routes WITHOUT payload (GET, HEAD, OPTIONS)
pub async fn run_py_handler_no_args(
    rt_handle: tokio::runtime::Handle,
    route_key: Arc<str>,
) -> Response {
    match rt_handle
        .spawn_blocking(move || {
            Python::attach(|py| {
                let entry = match ROUTES.get(route_key.as_ref()) {
                    Some(e) => e,
                    None => {
                        error!("Route handler not found: {}", route_key);
                        return (StatusCode::NOT_FOUND, "Route handler not found").into_response();
                    }
                };

                let handler = entry.value();

                match handler.func.call0(py) {
                    Ok(result) => {
                        let result_bound = result.into_bound(py);
                        py_to_response(py, &result_bound)
                    }
                    Err(err) => {
                        err.print(py);
                        error!("Error in route handler {}: {}", route_key, err);
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Error in route handler: {}", err),
                        )
                            .into_response()
                    }
                }
            })
        })
        .await
    {
        Ok(response) => response,
        Err(e) => {
            error!("Tokio spawn_blocking error: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
