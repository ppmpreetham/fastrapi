use crate::utils::{
    py_json_response_with_status, py_json_response_with_status_hint, py_to_response,
};
use axum::{
    body::Body,
    http::{HeaderName, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
};
use pyo3::{prelude::*, types::PyDict};
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
    class_name.is_some_and(|name| name == expected || name.rsplit('.').next() == Some(expected))
}

fn response_status(result: &Bound<'_, PyAny>, default: StatusCode) -> StatusCode {
    result
        .getattr("status_code")
        .ok()
        .and_then(|status| status.extract::<u16>().ok())
        .and_then(|status| StatusCode::from_u16(status).ok())
        .unwrap_or(default)
}

pub struct BaseResponseData<'py> {
    pub status_code: StatusCode,
    pub content: Option<Bound<'py, PyAny>>,
    pub headers: Option<Py<PyDict>>,
    pub media_type: Option<String>,
}

pub fn extract_base_response_data<'py>(
    result: &Bound<'py, PyAny>,
    default_status: StatusCode,
) -> BaseResponseData<'py> {
    let status_code = response_status(result, default_status);
    let content = result.getattr("content").ok();
    let headers = result
        .getattr("headers")
        .ok()
        .and_then(|h| h.extract::<Py<PyDict>>().ok());
    let media_type = result
        .getattr("media_type")
        .ok()
        .and_then(|m| m.extract::<String>().ok());
    BaseResponseData {
        status_code,
        content,
        headers,
        media_type,
    }
}

// wrapper classes

crate::define_response_class!(PyHTMLResponse, "HTMLResponse", String, 200);
crate::define_response_class!(PyJSONResponse, "JSONResponse", Py<PyAny>, 200);
crate::define_response_class!(PyORJSONResponse, "ORJSONResponse", Py<PyAny>, 200);
crate::define_response_class!(PyUJSONResponse, "UJSONResponse", Py<PyAny>, 200);
crate::define_response_class!(PyPlainTextResponse, "PlainTextResponse", String, 200);
crate::define_response_class!(PyStreamingResponse, "StreamingResponse", Py<PyAny>, 200);

#[pyclass(name = "RedirectResponse", get_all, set_all, from_py_object)]
#[derive(Clone)]
pub struct PyRedirectResponse {
    pub url: String,
    pub status_code: u16,
    pub headers: Option<Py<pyo3::types::PyDict>>,
    pub background: Option<Py<PyAny>>,
}

#[pymethods]
impl PyRedirectResponse {
    #[new]
    #[pyo3(signature = (url, status_code=307, headers=None, background=None))]
    fn new(
        url: String,
        status_code: u16,
        headers: Option<Py<pyo3::types::PyDict>>,
        background: Option<Py<PyAny>>,
    ) -> Self {
        Self {
            url,
            status_code,
            headers,
            background,
        }
    }
}

pub fn apply_response_metadata(
    py: Python<'_>,
    mut res: Response,
    headers: Option<&Py<PyDict>>,
    media_type: Option<&String>,
) -> Response {
    if let Some(headers) = headers
        && let Ok(dict) = headers.bind(py).cast::<PyDict>()
    {
        let headers_iter = dict.iter().filter_map(|(k, v)| {
            let ks = k.extract::<&str>().ok()?;
            let vs = v.extract::<&str>().ok()?;
            let hname = HeaderName::try_from(ks.as_bytes()).ok()?;
            let hval = HeaderValue::try_from(vs.as_bytes()).ok()?;
            Some((hname, hval))
        });

        res.headers_mut().extend(headers_iter);
    }

    if let Some(media_type) = media_type
        && let Ok(hval) = HeaderValue::try_from(media_type.as_str())
    {
        res.headers_mut()
            .insert(axum::http::header::CONTENT_TYPE, hval);
    }

    res
}

#[inline(always)]
pub fn convert_response_by_type(
    py: Python,
    result: &Bound<PyAny>,
    handler: &crate::routing::types::RouteHandler,
) -> PyResult<Response> {
    if result.is_none() {
        return Ok(handler
            .response
            .default_status
            .unwrap_or(axum::http::StatusCode::NO_CONTENT)
            .into_response());
    }

    let default_status = handler.response.default_status.unwrap_or(StatusCode::OK);
    let mut final_result = result;
    let validated_storage;

    if final_result.is_instance_of::<PyJSONResponse>() {
        return Ok(convert_json_response(py, final_result));
    } else if final_result.is_instance_of::<PyPlainTextResponse>() {
        return Ok(convert_text_response(py, final_result));
    } else if final_result.is_instance_of::<PyHTMLResponse>() {
        return Ok(convert_html_response(py, final_result));
    } else if final_result.is_instance_of::<PyRedirectResponse>() {
        return Ok(convert_redirect_response(py, final_result));
    } else if final_result.is_instance_of::<PyStreamingResponse>() {
        return Ok(convert_streaming_response(py, final_result));
    }

    let is_plain_value = final_result.is_instance_of::<pyo3::types::PyDict>()
        || final_result.is_instance_of::<pyo3::types::PyList>()
        || final_result.is_instance_of::<pyo3::types::PyString>()
        || final_result.is_instance_of::<pyo3::types::PyBool>()
        || final_result.is_instance_of::<pyo3::types::PyInt>()
        || final_result.is_instance_of::<pyo3::types::PyFloat>();

    let mut class_name = if !is_plain_value {
        let name = response_class_name(final_result);
        if let Some(ref name) = name
            && response_class_is(Some(name.as_str()), "Response")
        {
            return Ok(convert_auto_response(py, final_result));
        }
        name
    } else {
        None
    };

    if !is_plain_value {
        let has_body = final_result.getattr("body").is_ok();

        if has_body {
            return Ok(convert_auto_response(py, final_result));
        }
    }

    if !is_plain_value {
        if response_class_is(class_name.as_deref(), "JSONResponse")
            || response_class_is(class_name.as_deref(), "ORJSONResponse")
            || response_class_is(class_name.as_deref(), "UJSONResponse")
        {
            return Ok(convert_json_response(py, final_result));
        } else if response_class_is(class_name.as_deref(), "PlainTextResponse") {
            return Ok(convert_text_response(py, final_result));
        } else if response_class_is(class_name.as_deref(), "HTMLResponse") {
            return Ok(convert_html_response(py, final_result));
        } else if response_class_is(class_name.as_deref(), "RedirectResponse") {
            return Ok(convert_redirect_response(py, final_result));
        } else if response_class_is(class_name.as_deref(), "StreamingResponse") {
            return Ok(convert_streaming_response(py, final_result));
        }
    }

    if let Some(model) = &handler.response.response_model {
        validated_storage = model
            .bind(py)
            .call_method1("model_validate", (final_result,))?;
        final_result = &validated_storage;
        class_name = response_class_name(final_result);
    }

    let response = match handler.response.response_type {
        ResponseType::PlainText => {
            let body_bytes = final_result
                .extract::<&str>()
                .map(|s| bytes::Bytes::copy_from_slice(s.as_bytes()))
                .or_else(|_| {
                    final_result.str().and_then(|py_str| {
                        let s: &str = py_str.to_str()?;
                        Ok(bytes::Bytes::copy_from_slice(s.as_bytes()))
                    })
                })
                .unwrap_or_else(|_| bytes::Bytes::new());

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
            handler.response.serialization_hint,
        )?,

        ResponseType::Html => convert_html_response(py, final_result),

        ResponseType::Redirect => convert_redirect_response(py, final_result),

        ResponseType::Auto => {
            if final_result.is_instance_of::<PyJSONResponse>()
                || response_class_is(class_name.as_deref(), "JSONResponse")
                || response_class_is(class_name.as_deref(), "ORJSONResponse")
                || response_class_is(class_name.as_deref(), "UJSONResponse")
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
pub fn convert_html_response(py: Python, result: &Bound<PyAny>) -> Response {
    if let Ok(resp_bound) = result.cast::<PyHTMLResponse>() {
        let resp = resp_bound.borrow();
        let status_code = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK);
        let res = (status_code, Html(resp.content.clone())).into_response();
        apply_response_metadata(py, res, resp.headers.as_ref(), resp.media_type.as_ref())
    } else if response_class_is(response_class_name(result).as_deref(), "HTMLResponse") {
        let data = extract_base_response_data(result, StatusCode::OK);
        let content = data
            .content
            .and_then(|content| content.extract::<String>().ok())
            .unwrap_or_default();
        let res = (data.status_code, Html(content)).into_response();
        apply_response_metadata(py, res, data.headers.as_ref(), data.media_type.as_ref())
    } else {
        error!("Expected HTMLResponse, but got another type.");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

#[inline(always)]
pub fn convert_json_response(py: Python, result: &Bound<PyAny>) -> Response {
    if let Ok(resp_bound) = result.cast::<PyJSONResponse>() {
        let resp = resp_bound.borrow();
        let status_code = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK);
        let res = py_json_response_with_status(py, status_code, resp.content.bind(py))
            .unwrap_or_else(|err| {
                err.print(py);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            });
        apply_response_metadata(py, res, resp.headers.as_ref(), resp.media_type.as_ref())
    } else if let Ok(resp_bound) = result.cast::<PyORJSONResponse>() {
        let resp = resp_bound.borrow();
        let status_code = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK);
        let res = py_json_response_with_status(py, status_code, resp.content.bind(py))
            .unwrap_or_else(|err| {
                err.print(py);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            });
        apply_response_metadata(py, res, resp.headers.as_ref(), resp.media_type.as_ref())
    } else if let Ok(resp_bound) = result.cast::<PyUJSONResponse>() {
        let resp = resp_bound.borrow();
        let status_code = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK);
        let res = py_json_response_with_status(py, status_code, resp.content.bind(py))
            .unwrap_or_else(|err| {
                err.print(py);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            });
        apply_response_metadata(py, res, resp.headers.as_ref(), resp.media_type.as_ref())
    } else if response_class_is(response_class_name(result).as_deref(), "JSONResponse")
        || response_class_is(response_class_name(result).as_deref(), "ORJSONResponse")
        || response_class_is(response_class_name(result).as_deref(), "UJSONResponse")
    {
        let data = extract_base_response_data(result, StatusCode::OK);
        let res = match data.content {
            Some(ref content) => py_json_response_with_status(py, data.status_code, content)
                .unwrap_or_else(|err| {
                    err.print(py);
                    StatusCode::INTERNAL_SERVER_ERROR.into_response()
                }),
            None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
        apply_response_metadata(py, res, data.headers.as_ref(), data.media_type.as_ref())
    } else {
        error!("Expected JSONResponse, but got another type.");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

#[inline(always)]
pub fn convert_text_response(py: Python, result: &Bound<PyAny>) -> Response {
    if let Ok(resp_bound) = result.cast::<PyPlainTextResponse>() {
        let resp = resp_bound.borrow();
        let status_code = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK);
        let res = (
            status_code,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            )],
            resp.content.clone(),
        )
            .into_response();
        apply_response_metadata(py, res, resp.headers.as_ref(), resp.media_type.as_ref())
    } else if response_class_is(response_class_name(result).as_deref(), "PlainTextResponse") {
        let data = extract_base_response_data(result, StatusCode::OK);
        let content = data
            .content
            .and_then(|content| content.extract::<String>().ok())
            .unwrap_or_default();
        let res = (
            data.status_code,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            )],
            content,
        )
            .into_response();
        apply_response_metadata(py, res, data.headers.as_ref(), data.media_type.as_ref())
    } else {
        error!("Expected PlainTextResponse, but got another type.");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

#[inline(always)]
pub fn convert_redirect_response(py: Python, result: &Bound<PyAny>) -> Response {
    if let Ok(resp_bound) = result.cast::<PyRedirectResponse>() {
        let resp = resp_bound.borrow();
        let res = if resp.status_code == 301 {
            Redirect::permanent(&resp.url).into_response()
        } else {
            Redirect::temporary(&resp.url).into_response()
        };
        apply_response_metadata(py, res, resp.headers.as_ref(), None)
    } else if response_class_is(response_class_name(result).as_deref(), "RedirectResponse") {
        let url = result
            .getattr("url")
            .ok()
            .and_then(|url| url.extract::<String>().ok())
            .unwrap_or_default();
        let status = result
            .getattr("status_code")
            .ok()
            .and_then(|status| status.extract::<u16>().ok())
            .unwrap_or(307);
        let headers = result
            .getattr("headers")
            .ok()
            .and_then(|h| h.extract::<Py<PyDict>>().ok());
        let res = if status == 301 {
            Redirect::permanent(&url).into_response()
        } else {
            Redirect::temporary(&url).into_response()
        };
        apply_response_metadata(py, res, headers.as_ref(), None)
    } else {
        error!("Expected RedirectResponse, but got another type.");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

#[inline(always)]
pub fn convert_streaming_response(py: Python, result: &Bound<PyAny>) -> Response {
    let (content, status_code, headers, media_type) =
        if let Ok(resp_bound) = result.cast::<PyStreamingResponse>() {
            let resp = resp_bound.borrow();
            (
                resp.content.clone_ref(py),
                StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK),
                resp.headers.clone(),
                resp.media_type.clone(),
            )
        } else if response_class_is(response_class_name(result).as_deref(), "StreamingResponse") {
            let data = extract_base_response_data(result, StatusCode::OK);
            let content = data
                .content
                .map(|c| c.unbind())
                .unwrap_or_else(|| py.None());
            (content, data.status_code, data.headers, data.media_type)
        } else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };

    let locals = match rsloop::rust_async::get_current_locals(py) {
        Ok(l) => l,
        Err(e) => {
            e.print(py);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let stream = async_stream::stream! {
        let (is_async, is_sync) = Python::attach(|py| {
            let b = content.bind(py);
            (b.hasattr("__anext__").unwrap_or(false), b.hasattr("__next__").unwrap_or(false))
        });

        if is_async {
            loop {
                let fut = Python::attach(|py| {
                    match content.bind(py).call_method0("__anext__") {
                        Ok(awaitable) => {
                            rsloop::rust_async::into_future_with_locals(&locals, awaitable).ok()
                        }
                        Err(e) => {
                            if !e.is_instance_of::<pyo3::exceptions::PyStopAsyncIteration>(py) {
                                error!("StreamingResponse async error: {:?}", e);
                            }
                            None
                        }
                    }
                });
                let Some(fut) = fut else { break; };
                match fut.await {
                    Ok(val) => {
                        let bytes = Python::attach(|py| {
                            let bound = val.bind(py);
                            if let Ok(s) = bound.extract::<&str>() { Ok(bytes::Bytes::copy_from_slice(s.as_bytes())) }
                            else if let Ok(b) = bound.extract::<&[u8]>() { Ok(bytes::Bytes::copy_from_slice(b)) }
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
                let chunk = Python::attach(|py| {
                    match content.bind(py).call_method0("__next__") {
                        Ok(val) => {
                            if let Ok(s) = val.extract::<&str>() { Ok(Some(bytes::Bytes::copy_from_slice(s.as_bytes()))) }
                            else if let Ok(b) = val.extract::<&[u8]>() { Ok(Some(bytes::Bytes::copy_from_slice(b))) }
                            else { Err(()) }
                        }
                        Err(e) => {
                            if !e.is_instance_of::<pyo3::exceptions::PyStopIteration>(py) {
                                error!("StreamingResponse sync error: {:?}", e);
                            }
                            Ok(None)
                        }
                    }
                });
                match chunk {
                    Ok(Some(b)) => yield Ok::<_, std::convert::Infallible>(b),
                    Ok(None) => break,
                    Err(_) => continue,
                }
            }
        }
    };
    let res = (status_code, Body::from_stream(stream)).into_response();
    apply_response_metadata(py, res, headers.as_ref(), media_type.as_ref())
}

#[inline(always)]
pub fn convert_auto_response(py: Python, result: &Bound<PyAny>) -> Response {
    if result.is_none() {
        return StatusCode::NO_CONTENT.into_response();
    }

    let class_name = response_class_name(result);
    if result.is_instance_of::<PyJSONResponse>()
        || response_class_is(class_name.as_deref(), "JSONResponse")
        || response_class_is(class_name.as_deref(), "ORJSONResponse")
        || response_class_is(class_name.as_deref(), "UJSONResponse")
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
