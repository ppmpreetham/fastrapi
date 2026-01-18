use crate::dependencies::execute_dependencies;
use crate::pydantic::validate_with_pydantic;
use crate::responses::{PyHTMLResponse, PyJSONResponse, PyPlainTextResponse, PyRedirectResponse};
use crate::utils::local_guard;
use crate::utils::{json_to_py_object, py_any_to_json};
use crate::{ResponseType, ROUTES};
use axum::{
    http::{header, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    Json,
};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, error};

pub async fn run_py_handler_with_params(
    rt_handle: tokio::runtime::Handle,
    route_key: Arc<str>,
    path_params: HashMap<String, String>,
    query_params: HashMap<String, String>,
    payload: Option<serde_json::Value>,
) -> Response {
    let rt_handle_for_deps = rt_handle.clone();
    match rt_handle
        .spawn_blocking(move || {
            Python::attach(|py| {
                let guard = local_guard(&*ROUTES);
                let handler = match ROUTES.get(route_key.as_ref(), &guard) {
                    Some(h) => h,
                    None => return StatusCode::NOT_FOUND.into_response(),
                };

                let response_type = handler.response_type;
                let py_func = handler.func.bind(py);
                let kwargs = PyDict::new(py);

                // 1. Path Params
                for param_name in &handler.path_param_names {
                    if let Some(value) = path_params.get(param_name) {
                        kwargs.set_item(param_name, value).ok();
                    }
                }

                // 2. Query Params
                for param_name in &handler.query_param_names {
                    if let Some(value) = query_params.get(param_name) {
                        kwargs.set_item(param_name, value).ok();
                    }
                }

                // 3. Body / Pydantic Logic (THE FIX)
                if let Some(payload_val) = payload {
                    let validator_count = handler.param_validators.len();

                    if validator_count == 1 {
                        // CASE A: Single Pydantic Model -> Flat Body
                        // Expected JSON: { "name": "foo", "age": 10 }
                        let (param_name, validator) = &handler.param_validators[0];
                        let validator_bound = validator.bind(py);

                        // Pass the WHOLE payload, not a sub-key
                        match validate_with_pydantic(py, validator_bound, &payload_val) {
                            Ok(validated_model) => {
                                if let Err(e) = kwargs.set_item(param_name, validated_model) {
                                    error!("Failed to set body param: {}", e);
                                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                                }
                            }
                            Err(e) => return e, // Return validation error directly
                        }
                    } else if validator_count > 1 {
                        // CASE B: Multiple Pydantic Models -> Nested Body
                        // Expected JSON: { "user": { ... }, "item": { ... } }
                        let payload_obj = payload_val.as_object();

                        if let Some(obj) = payload_obj {
                            for (param_name, validator) in &handler.param_validators {
                                if let Some(param_data) = obj.get(param_name) {
                                    let validator_bound = validator.bind(py);
                                    match validate_with_pydantic(py, validator_bound, param_data) {
                                        Ok(validated_model) => {
                                            kwargs.set_item(param_name, validated_model).ok();
                                        }
                                        Err(e) => return e,
                                    }
                                } else {
                                    // Missing required key for multi-param body
                                    return (
                                        StatusCode::UNPROCESSABLE_ENTITY,
                                        Json(json!({
                                            "detail": format!("Missing body field: {}", param_name)
                                        })),
                                    )
                                        .into_response();
                                }
                            }
                        }
                    } else {
                        // CASE C: No Validators, but we have body params (raw dicts/args)
                        if let Some(obj) = payload_val.as_object() {
                            for param_name in &handler.body_param_names {
                                if let Some(val) = obj.get(param_name) {
                                    let py_val = json_to_py_object(py, val);
                                    kwargs.set_item(param_name, py_val).ok();
                                }
                            }
                        }
                    }
                }

                // 4. Dependencies
                if !handler.dependencies.is_empty() {
                    let request_dict = PyDict::new(py);
                    request_dict
                        .set_item("path_params", path_params.clone())
                        .ok();
                    request_dict
                        .set_item("query_params", query_params.clone())
                        .ok();
                    // Add headers/scope if needed

                    let deps_clone = handler.dependencies.clone();
                    let request_data: Py<PyDict> = request_dict.into();

                    let dep_results = rt_handle_for_deps.block_on(async move {
                        execute_dependencies(deps_clone, request_data).await
                    });

                    match dep_results {
                        Ok(results) => {
                            for (name, val) in results {
                                kwargs.set_item(name, val).ok();
                            }
                        }
                        Err(e) => {
                            e.print(py);
                            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                        }
                    }
                }

                // 5. Call Function
                let result = py_func.call((), Some(&kwargs));
                match result {
                    Ok(res) => convert_response_by_type(py, &res, response_type),
                    Err(e) => {
                        e.print(py);
                        if let Ok(exc) = e.value(py).extract::<crate::exceptions::PyHTTPException>()
                        {
                            return exc.to_response();
                        }
                        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
                    }
                }
            })
        })
        .await
    {
        Ok(response) => response,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

// Keep these wrappers for backward compatibility
pub async fn run_py_handler_with_args(
    rt_handle: tokio::runtime::Handle,
    route_key: Arc<str>,
    payload: serde_json::Value,
) -> Response {
    run_py_handler_with_params(
        rt_handle,
        route_key,
        HashMap::new(),
        HashMap::new(),
        Some(payload),
    )
    .await
}

pub async fn run_py_handler_no_args(
    rt_handle: tokio::runtime::Handle,
    route_key: Arc<str>,
) -> Response {
    run_py_handler_with_params(rt_handle, route_key, HashMap::new(), HashMap::new(), None).await
}

// Response conversion functions remain the same
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
        ResponseType::Auto => convert_auto_response(py, result),
    }
}

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

#[inline(always)]
fn convert_auto_response(py: Python, result: &Bound<PyAny>) -> Response {
    if result.is_none() {
        return StatusCode::NO_CONTENT.into_response();
    }
    let json = py_any_to_json(py, result);
    (StatusCode::OK, Json(json)).into_response()
}
