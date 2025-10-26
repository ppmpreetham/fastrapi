use crate::pydantic::validate_with_pydantic;
use crate::{
    utils::{json_to_py_object, py_to_response},
    ROUTES,
};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::json;
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

                let payload_obj = match payload.as_object() {
                    Some(obj) => obj,
                    None => {
                        return (
                            StatusCode::UNPROCESSABLE_ENTITY,
                            Json(json!({"detail": "Payload must be an object for this route"})),
                        )
                            .into_response();
                    }
                };

                let kwargs = PyDict::new(py);

                // Use cached param_validators instead of re-parsing annotations
                if !handler.param_validators.is_empty() {
                    // We have Pydantic validators cached - validate each parameter
                    for (param_name, validator) in &handler.param_validators {
                        if let Some(param_data) = payload_obj.get(param_name) {
                            let validator_bound = validator.bind(py);
                            match validate_with_pydantic(py, validator_bound, param_data) {
                                Ok(validated_model) => {
                                    if let Err(e) =
                                        kwargs.set_item(param_name.as_str(), validated_model)
                                    {
                                        error!("Failed to set kwarg '{}': {}", param_name, e);
                                        return (
                                            StatusCode::INTERNAL_SERVER_ERROR,
                                            "Failed to set parameter",
                                        )
                                            .into_response();
                                    }
                                }
                                Err(err_resp) => return err_resp,
                            }
                        } else {
                            return (
                                StatusCode::UNPROCESSABLE_ENTITY,
                                Json(json!({
                                    "detail": format!("Missing required parameter: {}", param_name)
                                })),
                            )
                                .into_response();
                        }
                    }
                } else {
                    // No Pydantic validators - pass payload fields as-is
                    for (key, value) in payload_obj.iter() {
                        let py_value = json_to_py_object(py, value);
                        if let Err(e) = kwargs.set_item(key, py_value) {
                            error!("Failed to set kwarg '{}': {}", key, e);
                        }
                    }
                }

                let result = py_func.call((), Some(&kwargs));

                match result {
                    Ok(result) => py_to_response(py, &result),
                    Err(err) => {
                        #[cfg(debug_assertions)]
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
                        #[cfg(debug_assertions)]
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
