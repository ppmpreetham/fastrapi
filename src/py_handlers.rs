use crate::pydantic::{is_pydantic_model, validate_with_pydantic};
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
use pyo3::types::{PyDict};
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

                match py_func.getattr("__annotations__") {
                    Ok(annotations) => {
                        if let Ok(ann_dict) = annotations.cast::<PyDict>() {
                            for (key, value) in ann_dict.iter() {
                                let param_name = match key.extract::<String>() {
                                    Ok(name) => name,
                                    Err(_) => continue,
                                };
                                
                                if param_name == "return" {
                                    continue;
                                }

                                if is_pydantic_model(py, &value) {
                                    if let Some(param_data) = payload_obj.get(&param_name) {
                                        match validate_with_pydantic(py, &value, param_data) {
                                            Ok(validated_model) => {
                                                kwargs.set_item(key, validated_model).unwrap();
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
                                } else {
                                    // non-Pydantic parameters, if any
                                    if let Some(param_data) = payload_obj.get(&param_name) {
                                        let py_param = json_to_py_object(py, param_data);
                                        kwargs.set_item(key, py_param).unwrap();
                                    }
                                }
                            }
                        }
                    }
                    Err(_) => {
                        // fallback for no annotations or other issues, pass the whole payload
                        let py_payload = json_to_py_object(py, &payload);
                        kwargs.set_item("payload", py_payload).unwrap();
                    }
                };

                let result = py_func.call((), Some(&kwargs));

                match result {
                    Ok(result) => py_to_response(py, &result),
                    Err(err) => {
                        // #[cfg(debug_assertions)]
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
