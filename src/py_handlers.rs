use crate::responses::{PyHTMLResponse, PyJSONResponse, PyPlainTextResponse, PyRedirectResponse};
use crate::utils::py_any_to_json;
use crate::{ResponseType, RouteHandler, ROUTES};
use axum::{
    http::{header, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    Json,
};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::error;

pub async fn run_py_handler_with_params(
    rt_handle: tokio::runtime::Handle,
    route_key: Arc<str>,
    path_params: HashMap<String, String>,
    query_params: HashMap<String, String>,
    payload: Option<serde_json::Value>,
) -> Response {
    match rt_handle
        .spawn_blocking(move || {
            Python::attach(|py| {
                let guard = crate::utils::local_guard(&*ROUTES);
                let handler = match ROUTES.get(route_key.as_ref(), &guard) {
                    Some(h) => h,
                    None => return StatusCode::NOT_FOUND.into_response(),
                };

                let response_type = handler.response_type;
                let py_func = handler.func.bind(py);

                let kwargs = if handler.needs_kwargs {
                    let dict = PyDict::new(py);

                    // path params
                    for (k, v) in &path_params {
                        dict.set_item(k, v).ok();
                    }

                    // query params
                    for (k, v) in &query_params {
                        dict.set_item(k, v).ok();
                    }

                    // body / validation
                    if let Some(payload_val) = &payload {
                        if let Err(resp) = crate::pydantic::apply_body_and_validation(
                            py,
                            handler,
                            payload_val,
                            &dict,
                        ) {
                            return resp;
                        }
                    }

                    Some(dict)
                } else {
                    None
                };

                // PYTHON CALLING ONLY ONCE
                let result = match kwargs {
                    Some(ref kw) => py_func.call((), Some(kw)),
                    None => py_func.call0(),
                };

                match result {
                    Ok(res) => convert_response_by_type(py, &res, response_type),
                    Err(err) => {
                        err.print(py);
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    }
                }
            })
        })
        .await
    {
        Ok(resp) => resp,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn run_slow_handler(
    py: Python,
    handler: &RouteHandler,
    path_params: HashMap<String, String>,
    query_params: HashMap<String, String>,
    payload: Option<serde_json::Value>,
) -> Response {
    let py_func = handler.func.bind(py);

    let kwargs = if handler.needs_kwargs {
        Some(PyDict::new(py))
    } else {
        None
    };

    if let Some(kwargs) = kwargs {
        for (k, v) in path_params {
            kwargs.set_item(k, v).ok();
        }
        for (k, v) in query_params {
            kwargs.set_item(k, v).ok();
        }

        let _payload = payload;

        match py_func.call((), Some(&kwargs)) {
            Ok(res) => convert_response_by_type(py, &res, handler.response_type),
            Err(e) => {
                e.print(py);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    } else {
        match py_func.call0() {
            Ok(res) => convert_response_by_type(py, &res, handler.response_type),
            Err(e) => {
                e.print(py);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

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
