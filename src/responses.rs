use crate::utils::py_any_to_json;
use axum::{
    http::{header, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    Json,
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
    response_type: ResponseType,
) -> Response {
    if result.is_none() {
        return StatusCode::NO_CONTENT.into_response();
    }

    if result.is_instance_of::<PyJSONResponse>() {
        return convert_json_response(py, result);
    } else if result.is_instance_of::<PyPlainTextResponse>() {
        return convert_text_response(py, result);
    } else if result.is_instance_of::<PyHTMLResponse>() {
        return convert_html_response(py, result);
    } else if result.is_instance_of::<PyRedirectResponse>() {
        return convert_redirect_response(py, result);
    }

    match response_type {
        ResponseType::Json => {
            let json = crate::utils::py_any_to_json(py, result);
            (StatusCode::OK, Json(json)).into_response()
        }
        ResponseType::PlainText => {
            let text = result
                .extract::<String>()
                .unwrap_or_else(|_| result.to_string());
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                text,
            )
                .into_response()
        }
        ResponseType::Html => convert_html_response(py, result),
        ResponseType::Redirect => convert_redirect_response(py, result),
        ResponseType::Auto => crate::utils::py_to_response(py, result),
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
        (status_code, Json(json)).into_response()
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
    (StatusCode::OK, Json(json)).into_response()
}
