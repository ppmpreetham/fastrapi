use crate::pydantic::validate_with_pydantic;
use crate::utils::{json_to_py_object, py_any_to_json};
use crate::{PyHTMLResponse, PyJSONResponse, PyPlainTextResponse, PyRedirectResponse};
use crate::{ResponseType, ROUTES};
use axum::{
    http::{header, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    Json,
};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::json;
use std::sync::Arc;
use tracing::error;

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
                let response_type = handler.response_type;
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

                if !handler.param_validators.is_empty() {
                    for (param_name, validator, method) in &handler.param_validators {
                        if let Some(param_data) = payload_obj.get(param_name) {
                            let validator_bound = validator.bind(py);
                            match validate_with_pydantic(py, validator_bound, param_data, *method) {
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
                    for (key, value) in payload_obj.iter() {
                        let py_value = json_to_py_object(py, value);
                        if let Err(e) = kwargs.set_item(key, py_value) {
                            error!("Failed to set kwarg '{}': {}", key, e);
                        }
                    }
                }

                let result = py_func.call((), Some(&kwargs));

                match result {
                    Ok(result) => convert_response_by_type(py, &result, response_type),
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
                let response_type = handler.response_type;

                match handler.func.call0(py) {
                    Ok(result) => {
                        let result_bound = result.into_bound(py);
                        convert_response_by_type(py, &result_bound, response_type)
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

// branch ONCE based on pre-determined response type
#[inline(always)]
fn convert_response_by_type(
    py: Python,
    result: &Bound<PyAny>,
    response_type: ResponseType,
) -> Response {
    match response_type {
        ResponseType::Html => convert_html_response(py, result),
        ResponseType::Json => convert_json_response(py, result),
        ResponseType::PlainText => convert_text_response(py, result),
        ResponseType::Redirect => convert_redirect_response(py, result),
        ResponseType::Auto => convert_auto_response(py, result), // untyped
    }
}

// Use Axum's Html - direct attribute access (no type checking)
#[inline(always)]
fn convert_html_response(_py: Python, result: &Bound<PyAny>) -> Response {
    if let Ok(resp) = result.extract::<PyRef<'_, PyHTMLResponse>>() {
        let status_code = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK);
        (status_code, Html(resp.content.clone())).into_response()
    } else {
        error!("Expected HTMLResponse, but got another type.");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

// Use Axum's Json - direct attribute access
#[inline(always)]
fn convert_json_response(py: Python, result: &Bound<PyAny>) -> Response {
    if let Ok(resp) = result.extract::<PyRef<'_, PyJSONResponse>>() {
        let status_code = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK);
        let json = py_any_to_json(py, &resp.content.bind(py));
        (status_code, Json(json)).into_response()
    } else {
        error!("Expected JSONResponse, but got another type.");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

// Plain text using Axum's tuple response
#[inline(always)]
fn convert_text_response(_py: Python, result: &Bound<PyAny>) -> Response {
    if let Ok(resp) = result.extract::<PyRef<'_, PyPlainTextResponse>>() {
        let status_code = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK);
        (
            status_code,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            resp.content.clone(),
        )
            .into_response()
    } else {
        error!("Expected PlainTextResponse, but got another type.");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

// Use Axum's Redirect
#[inline(always)]
fn convert_redirect_response(_py: Python, result: &Bound<PyAny>) -> Response {
    if let Ok(resp) = result.extract::<PyRef<'_, PyRedirectResponse>>() {
        if resp.status_code == 301 {
            Redirect::permanent(&resp.url).into_response()
        } else {
            Redirect::temporary(&resp.url).into_response()
        }
    } else {
        error!("Expected RedirectResponse, but got another type.");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

// for untyped responses - FAST PATH
#[inline(always)]
fn convert_auto_response(py: Python, result: &Bound<PyAny>) -> Response {
    if result.is_none() {
        return StatusCode::NO_CONTENT.into_response();
    }
    // Direct JSON conversion
    let json = py_any_to_json(py, result);
    (StatusCode::OK, Json(json)).into_response()
}
