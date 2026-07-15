use crate::utils::{
    py_json_response_with_status, py_json_response_with_status_hint, py_to_response,
};
use axum::{
    body::Body,
    http::{HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
};
use pyo3::prelude::*;
use tracing::error;

use pyo3::{Py, PyAny, pyclass, pymethods};

use crate::types::response::ResponseType;

fn response_class_name(result: &Bound<'_, PyAny>) -> Option<String> {
    result
        .get_type()
        .name()
        .ok()
        .and_then(|name| name.to_str().ok().map(str::to_owned))
}

fn response_class_is(class_name: Option<&str>, expected: &str) -> bool {
    class_name
        .map(|name| name == expected || name.rsplit('.').next() == Some(expected))
        .unwrap_or(false)
}

fn response_status(result: &Bound<'_, PyAny>, default: StatusCode) -> StatusCode {
    result
        .getattr("status_code")
        .ok()
        .and_then(|status| status.extract::<u16>().ok())
        .and_then(|status| StatusCode::from_u16(status).ok())
        .unwrap_or(default)
}

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

#[pyclass(name = "StreamingResponse", skip_from_py_object)]
#[derive(Clone)]
pub struct PyStreamingResponse {
    #[pyo3(get)]
    pub content: Py<PyAny>,
    #[pyo3(get)]
    pub status_code: u16,
}

#[pymethods]
impl PyStreamingResponse {
    #[new]
    #[pyo3(signature = (content, status_code=200))]
    fn new(content: Py<PyAny>, status_code: u16) -> Self {
        Self {
            content,
            status_code,
        }
    }
}

#[inline(always)]
pub fn convert_response_by_type(
    py: Python,
    result: &Bound<PyAny>,
    handler: &crate::routing::types::RouteHandler,
) -> PyResult<Response> {
    if result.is_none() {
        return Ok(handler
            .default_status
            .unwrap_or(StatusCode::NO_CONTENT)
            .into_response());
    }

    let default_status = handler.default_status.unwrap_or(StatusCode::OK);

    let mut final_result = result;

    let validated_storage;

    let class_name = response_class_name(final_result);
    let is_explicit_response = final_result.is_instance_of::<PyJSONResponse>()
        || final_result.is_instance_of::<PyPlainTextResponse>()
        || final_result.is_instance_of::<PyHTMLResponse>()
        || final_result.is_instance_of::<PyRedirectResponse>()
        || final_result.is_instance_of::<PyStreamingResponse>()
        || response_class_is(class_name.as_deref(), "JSONResponse")
        || response_class_is(class_name.as_deref(), "PlainTextResponse")
        || response_class_is(class_name.as_deref(), "HTMLResponse")
        || response_class_is(class_name.as_deref(), "RedirectResponse")
        || response_class_is(class_name.as_deref(), "StreamingResponse");

    if let Some(model) = &handler.response_model {
        if !is_explicit_response {
            validated_storage = model
                .bind(py)
                .call_method1("model_validate", (final_result,))?;
            final_result = &validated_storage;
        }
    }

    if is_explicit_response {
        if final_result.is_instance_of::<PyJSONResponse>()
            || response_class_is(class_name.as_deref(), "JSONResponse")
        {
            return Ok(convert_json_response(py, final_result));
        } else if final_result.is_instance_of::<PyPlainTextResponse>()
            || response_class_is(class_name.as_deref(), "PlainTextResponse")
        {
            return Ok(convert_text_response(py, final_result));
        } else if final_result.is_instance_of::<PyHTMLResponse>()
            || response_class_is(class_name.as_deref(), "HTMLResponse")
        {
            return Ok(convert_html_response(py, final_result));
        } else if final_result.is_instance_of::<PyRedirectResponse>()
            || response_class_is(class_name.as_deref(), "RedirectResponse")
        {
            return Ok(convert_redirect_response(py, final_result));
        } else if final_result.is_instance_of::<PyStreamingResponse>()
            || response_class_is(class_name.as_deref(), "StreamingResponse")
        {
            return Ok(convert_streaming_response(py, final_result));
        }
    }

    let response = match handler.response_type {
        ResponseType::PlainText => {
            // Fast native UTF-8 extraction path
            let body_bytes = if let Ok(s) = final_result.extract::<&str>() {
                bytes::Bytes::copy_from_slice(s.as_bytes())
            } else {
                match final_result.str() {
                    Ok(py_str) => match py_str.to_str() {
                        Ok(s) => bytes::Bytes::copy_from_slice(s.as_bytes()),
                        Err(_) => bytes::Bytes::new(),
                    },
                    Err(_) => bytes::Bytes::new(),
                }
            };

            (
                default_status,
                [(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/plain; charset=utf-8"),
                )],
                body_bytes,
            )
                .into_response()
        }

        ResponseType::Json => py_json_response_with_status_hint(
            py,
            default_status,
            final_result,
            handler.serialization_hint,
        )?,

        ResponseType::Html => convert_html_response(py, final_result),

        ResponseType::Redirect => convert_redirect_response(py, final_result),

        ResponseType::Auto => {
            // cannot determine response type AOT.

            let class_name = response_class_name(final_result);

            if final_result.is_instance_of::<PyJSONResponse>()
                || response_class_is(class_name.as_deref(), "JSONResponse")
            {
                convert_json_response(py, final_result)
            } else if final_result.is_instance_of::<PyPlainTextResponse>()
                || response_class_is(class_name.as_deref(), "PlainTextResponse")
            {
                convert_text_response(py, final_result)
            } else if final_result.is_instance_of::<PyHTMLResponse>()
                || response_class_is(class_name.as_deref(), "HTMLResponse")
            {
                convert_html_response(py, final_result)
            } else if final_result.is_instance_of::<PyRedirectResponse>()
                || response_class_is(class_name.as_deref(), "RedirectResponse")
            {
                convert_redirect_response(py, final_result)
            } else if final_result.is_instance_of::<PyStreamingResponse>()
                || response_class_is(class_name.as_deref(), "StreamingResponse")
            {
                convert_streaming_response(py, final_result)
            } else {
                py_to_response(py, final_result, default_status)
            }
        }
    };

    Ok(response)
}

#[inline(always)]
pub fn convert_html_response(_py: Python, result: &Bound<PyAny>) -> Response {
    if let Ok(resp) = result.extract::<PyRef<'_, PyHTMLResponse>>() {
        let status_code = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK);
        (status_code, Html(resp.content.clone())).into_response()
    } else if response_class_is(response_class_name(result).as_deref(), "HTMLResponse") {
        let status_code = response_status(result, StatusCode::OK);
        let content = result
            .getattr("content")
            .ok()
            .and_then(|content| content.extract::<String>().ok())
            .unwrap_or_default();
        (status_code, Html(content)).into_response()
    } else {
        error!("Expected HTMLResponse, but got another type.");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

#[inline(always)]
pub fn convert_json_response(py: Python, result: &Bound<PyAny>) -> Response {
    if let Ok(resp) = result.extract::<PyRef<'_, PyJSONResponse>>() {
        let status_code = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK);
        py_json_response_with_status(py, status_code, resp.content.bind(py)).unwrap_or_else(|err| {
            err.print(py);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })
    } else if response_class_is(response_class_name(result).as_deref(), "JSONResponse") {
        let status_code = response_status(result, StatusCode::OK);
        match result.getattr("content") {
            Ok(content) => {
                py_json_response_with_status(py, status_code, &content).unwrap_or_else(|err| {
                    err.print(py);
                    StatusCode::INTERNAL_SERVER_ERROR.into_response()
                })
            }
            Err(err) => {
                err.print(py);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
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
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            )],
            resp.content.clone(),
        )
            .into_response()
    } else if response_class_is(response_class_name(result).as_deref(), "PlainTextResponse") {
        let status_code = response_status(result, StatusCode::OK);
        let content = result
            .getattr("content")
            .ok()
            .and_then(|content| content.extract::<String>().ok())
            .unwrap_or_default();
        (
            status_code,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            )],
            content,
        )
            .into_response()
    } else {
        error!("Expected PlainTextResponse, but got another type.");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

#[inline(always)]
pub fn convert_redirect_response(_py: Python, result: &Bound<PyAny>) -> Response {
    let redirect = if let Ok(resp) = result.extract::<PyRef<'_, PyRedirectResponse>>() {
        Some((resp.url.clone(), resp.status_code))
    } else if response_class_is(response_class_name(result).as_deref(), "RedirectResponse") {
        result
            .getattr("url")
            .ok()
            .and_then(|url| url.extract::<String>().ok())
            .map(|url| {
                let status = result
                    .getattr("status_code")
                    .ok()
                    .and_then(|status| status.extract::<u16>().ok())
                    .unwrap_or(307);
                (url, status)
            })
    } else {
        None
    };

    if let Some((url, status_code)) = redirect {
        if status_code == 301 {
            Redirect::permanent(&url).into_response()
        } else {
            Redirect::temporary(&url).into_response()
        }
    } else {
        error!("Expected RedirectResponse, but got another type.");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

#[inline(always)]
pub fn convert_streaming_response(py: Python, result: &Bound<PyAny>) -> Response {
    let (content, status_code) =
        if let Ok(resp) = result.extract::<PyRef<'_, PyStreamingResponse>>() {
            (
                resp.content.clone_ref(py),
                StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK),
            )
        } else if response_class_is(response_class_name(result).as_deref(), "StreamingResponse") {
            let status_code = response_status(result, StatusCode::OK);
            let content = result
                .getattr("content")
                .ok()
                .map(|c| c.unbind())
                .unwrap_or_else(|| py.None());
            (content, status_code)
        } else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };

    let locals = rsloop::rust_async::get_current_locals(py).unwrap();

    let stream = async_stream::stream! {
        let (is_async, is_sync) = Python::attach(|py| {
            let b = content.bind(py);
            (b.hasattr("__anext__").unwrap_or(false), b.hasattr("__next__").unwrap_or(false))
        });

        if is_async {
            loop {
                let awaitable = Python::attach(|py| content.bind(py).call_method0("__anext__").map(|a| a.unbind()));
                let awaitable = match awaitable {
                    Ok(a) => a,
                    Err(e) => {
                        Python::attach(|py| if !e.is_instance_of::<pyo3::exceptions::PyStopAsyncIteration>(py) { error!("StreamingResponse async error: {:?}", e); });
                        break;
                    }
                };
                let fut = match Python::attach(|py| rsloop::rust_async::into_future_with_locals(&locals, awaitable.bind(py).clone())) {
                    Ok(f) => f,
                    Err(_) => break,
                };
                match fut.await {
                    Ok(val) => {
                        let bytes = Python::attach(|py| {
                            let bound = val.bind(py);
                            if let Ok(s) = bound.extract::<String>() { Ok(bytes::Bytes::from(s)) }
                            else if let Ok(b) = bound.extract::<&[u8]>() { Ok(bytes::Bytes::from(b.to_vec())) }
                            else { Err(()) }
                        });
                        if let Ok(b) = bytes { yield Ok::<_, std::convert::Infallible>(b); }
                    }
                    Err(e) => {
                        Python::attach(|py| if !e.is_instance_of::<pyo3::exceptions::PyStopAsyncIteration>(py) { error!("StreamingResponse async error: {:?}", e); });
                        break;
                    }
                }
            }
        } else if is_sync {
            loop {
                let val = Python::attach(|py| content.bind(py).call_method0("__next__").map(|a| a.unbind()));
                match val {
                    Ok(val) => {
                        let bytes = Python::attach(|py| {
                            let bound = val.bind(py);
                            if let Ok(s) = bound.extract::<String>() { Ok(bytes::Bytes::from(s)) }
                            else if let Ok(b) = bound.extract::<&[u8]>() { Ok(bytes::Bytes::from(b.to_vec())) }
                            else { Err(()) }
                        });
                        if let Ok(b) = bytes { yield Ok::<_, std::convert::Infallible>(b); }
                    }
                    Err(e) => {
                        Python::attach(|py| if !e.is_instance_of::<pyo3::exceptions::PyStopIteration>(py) { error!("StreamingResponse sync error: {:?}", e); });
                        break;
                    }
                }
            }
        }
    };
    (status_code, Body::from_stream(stream)).into_response()
}

#[inline(always)]
pub fn convert_auto_response(py: Python, result: &Bound<PyAny>) -> Response {
    if result.is_none() {
        return StatusCode::NO_CONTENT.into_response();
    }

    let class_name = response_class_name(result);
    if result.is_instance_of::<PyJSONResponse>()
        || response_class_is(class_name.as_deref(), "JSONResponse")
    {
        return convert_json_response(py, result);
    }
    if result.is_instance_of::<PyPlainTextResponse>()
        || response_class_is(class_name.as_deref(), "PlainTextResponse")
    {
        return convert_text_response(py, result);
    }
    if result.is_instance_of::<PyHTMLResponse>()
        || response_class_is(class_name.as_deref(), "HTMLResponse")
    {
        return convert_html_response(py, result);
    }
    if result.is_instance_of::<PyRedirectResponse>()
        || response_class_is(class_name.as_deref(), "RedirectResponse")
    {
        return convert_redirect_response(py, result);
    }
    if result.is_instance_of::<PyStreamingResponse>()
        || response_class_is(class_name.as_deref(), "StreamingResponse")
    {
        return convert_streaming_response(py, result);
    }

    crate::utils::py_json_response(py, result).unwrap_or_else(|err| {
        err.print(py);
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })
}
