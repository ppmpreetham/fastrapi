use crate::utils::utils::{json_response, json_response_with_status, py_any_to_json, py_to_response};
use axum::{
    http::{header, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
};
use pyo3::prelude::*;
use tracing::error;

use pyo3::{pyclass, pymethods, Py, PyAny};

use crate::types::response::ResponseType;

// wrapper classes
#[pyclass(name = "HTMLResponse", skip_from_py_object)]
#[derive(Clone)]
pub struct PyHTMLResponse {
    #[pyo3(get)]
    pub content: String,
    #[pyo3(get)]
    pub status_code: u16,
}

#[pymethods]
impl PyHTMLResponse {
    #[new]
    #[pyo3(signature = (content, status_code=200))]
    fn new(content: String, status_code: u16) -> Self {
        Self {
            content,
            status_code,
        }
    }
}

#[pyclass(name = "JSONResponse", skip_from_py_object)]
#[derive(Clone)]
pub struct PyJSONResponse {
    #[pyo3(get)]
    pub content: Py<PyAny>,
    #[pyo3(get)]
    pub status_code: u16,
}

#[pymethods]
impl PyJSONResponse {
    #[new]
    #[pyo3(signature = (content, status_code=200))]
    fn new(content: Py<PyAny>, status_code: u16) -> Self {
        Self {
            content,
            status_code,
        }
    }
}

#[pyclass(name = "PlainTextResponse", skip_from_py_object)]
#[derive(Clone)]
pub struct PyPlainTextResponse {
    #[pyo3(get)]
    pub content: String,
    #[pyo3(get)]
    pub status_code: u16,
}

#[pymethods]
impl PyPlainTextResponse {
    #[new]
    #[pyo3(signature = (content, status_code=200))]
    fn new(content: String, status_code: u16) -> Self {
        Self {
            content,
            status_code,
        }
    }
}

#[pyclass(name = "RedirectResponse", skip_from_py_object)]
#[derive(Clone)]
pub struct PyRedirectResponse {
    #[pyo3(get)]
    pub url: String,
    #[pyo3(get)]
    pub status_code: u16,
}

#[pymethods]
impl PyRedirectResponse {
    #[new]
    #[pyo3(signature = (url, status_code=307))]
    fn new(url: String, status_code: u16) -> Self {
        Self { url, status_code }
    }
}

#[inline(always)]
pub fn convert_response_by_type(
    py: Python,
    result: &Bound<PyAny>,
    handler: &crate::routing::types::RouteHandler,
) -> Response {
    let mut final_result = result.clone();

    let is_explicit_response = final_result.is_instance_of::<PyJSONResponse>() ||
                               final_result.is_instance_of::<PyPlainTextResponse>() ||
                               final_result.is_instance_of::<PyHTMLResponse>() ||
                               final_result.is_instance_of::<PyRedirectResponse>();

    if !is_explicit_response && !final_result.is_none() {
        if let Some(model) = &handler.response_model {
            if let Ok(validated) = model.bind(py).call_method1("model_validate", (&final_result,)) {
                if let Ok(dumped) = validated.call_method0("model_dump") {
                    final_result = dumped;
                }
            }
        }
    }

    if final_result.is_none() {
        let status = handler.default_status.unwrap_or(StatusCode::NO_CONTENT);
        return status.into_response();
    }

    if final_result.is_instance_of::<PyJSONResponse>() {
        return convert_json_response(py, &final_result);
    } else if final_result.is_instance_of::<PyPlainTextResponse>() {
        return convert_text_response(py, &final_result);
    } else if final_result.is_instance_of::<PyHTMLResponse>() {
        return convert_html_response(py, &final_result);
    } else if final_result.is_instance_of::<PyRedirectResponse>() {
        return convert_redirect_response(py, &final_result);
    }

    let default_status = handler.default_status.unwrap_or(StatusCode::OK);

    match handler.response_type {
        ResponseType::Json => {
            let json = py_any_to_json(py, &final_result);
            crate::utils::utils::json_response_with_status(py, default_status, &json)
        }
        ResponseType::PlainText => {
            let text = final_result
                .extract::<String>()
                .unwrap_or_else(|_| final_result.to_string());
            (
                default_status,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                text,
            )
                .into_response()
        }
        ResponseType::Html => convert_html_response(py, &final_result),
        ResponseType::Redirect => convert_redirect_response(py, &final_result),
        ResponseType::Auto => py_to_response(py, &final_result, default_status),
    }
}

#[inline(always)]
pub fn convert_html_response(_py: Python, result: &Bound<PyAny>) -> Response {
    if let Ok(resp) = result.extract::<PyRef<'_, PyHTMLResponse>>() {
        let status_code = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK);
        (status_code, Html(resp.content.clone())).into_response()
    } else {
        error!("Expected HTMLResponse, but got another type.");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

#[inline(always)]
pub fn convert_json_response(py: Python, result: &Bound<PyAny>) -> Response {
    if let Ok(resp) = result.extract::<PyRef<'_, PyJSONResponse>>() {
        let status_code = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK);
        let json = py_any_to_json(py, &resp.content.bind(py));
        json_response_with_status(py, status_code, &json)
    } else {
        error!("Expected JSONResponse, but got another type.");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

#[inline(always)]
pub fn convert_text_response(_py: Python, result: &Bound<PyAny>) -> Response {
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
pub fn convert_redirect_response(_py: Python, result: &Bound<PyAny>) -> Response {
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
pub fn convert_auto_response(py: Python, result: &Bound<PyAny>) -> Response {
    if result.is_none() {
        return StatusCode::NO_CONTENT.into_response();
    }
    let json = py_any_to_json(py, result);
    json_response(py, &json)
}
