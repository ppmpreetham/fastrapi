use super::response_file::convert_file_response;
use super::responses::{
    PyFileResponse, PyRedirectResponse, ResponseKind, extract_base_response_data,
    is_starlette_response, response_kind, response_status, validate_response_model,
};
use crate::background::{spawn_response_background, spawn_response_background_value};
use crate::response_type::{ResponseFormatter, ResponseType};
use crate::responses::{
    PyHTMLResponse, PyJSONResponse, PyORJSONResponse, PyPlainTextResponse, PyStreamingResponse,
    PyUJSONResponse,
};
use crate::sse::PyEventSourceResponse;
use crate::sse::{event_fields, format_sse_event, sse_options};
use axum::body::Body;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use bytes::Bytes;
use fastrapi_pyutil::json::{
    py_json_response_with_dump_options, py_json_response_with_status, py_to_response,
};
use fastrapi_runtime::py_bridge;
use fastrapi_runtime::server_state::server_loop;
use pyo3::exceptions::{PyStopAsyncIteration, PyStopIteration};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use pyo3::{Bound, Py, PyAny, intern};
use pyo3_async_runtimes::TaskLocals;
use std::collections::HashSet;
use std::convert::Infallible;
use std::time::Duration;
use tokio::time::timeout;
use tracing::error;

pub fn apply_response_metadata(
    py: Python<'_>,
    mut res: Response,
    headers: Option<&Py<PyDict>>,
    media_type: Option<&str>,
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
        && let Ok(hval) = HeaderValue::try_from(media_type)
    {
        res.headers_mut().insert(CONTENT_TYPE, hval);
    }

    res
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RawHeaders {
    All,
    CookiesOnly,
}

pub(crate) fn apply_raw_headers(
    mut res: Response,
    result: &Bound<'_, PyAny>,
    which: RawHeaders,
) -> Response {
    let Ok(raw) = result.getattr("raw_headers") else {
        return res;
    };
    let Ok(raw) = raw.cast::<PyList>() else {
        return res;
    };
    let mut replaced = HashSet::new();
    for item in raw.iter() {
        let Ok(item) = item.cast::<PyTuple>() else {
            continue;
        };
        let Ok(name_item) = item.get_item(0) else {
            continue;
        };
        let Ok(value_item) = item.get_item(1) else {
            continue;
        };
        let Ok(name) = name_item.extract::<&[u8]>() else {
            continue;
        };
        let Ok(value) = value_item.extract::<&[u8]>() else {
            continue;
        };
        if which == RawHeaders::CookiesOnly && !name.eq_ignore_ascii_case(b"set-cookie") {
            continue;
        }
        if let (Ok(name), Ok(value)) =
            (HeaderName::from_bytes(name), HeaderValue::from_bytes(value))
        {
            if replaced.insert(name.clone()) {
                res.headers_mut().remove(&name);
            }
            res.headers_mut().append(name, value);
        }
    }
    res
}

fn rendered_response(result: &Bound<'_, PyAny>) -> Option<Response> {
    let body = result.getattr("body").ok().filter(|body| !body.is_none())?;
    let body = if let Ok(bytes) = body.extract::<&[u8]>() {
        Bytes::copy_from_slice(bytes)
    } else {
        let bytes = body.call_method0("tobytes").ok()?;
        Bytes::copy_from_slice(bytes.extract::<&[u8]>().ok()?)
    };
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = response_status(result, StatusCode::OK);
    Some(apply_raw_headers(response, result, RawHeaders::All))
}

#[inline(always)]
pub fn convert_response_by_type(
    py: Python,
    result: &Bound<PyAny>,
    formatter: &ResponseFormatter,
    request_headers: Option<&HeaderMap>,
) -> PyResult<Response> {
    if result.is_none() {
        return Ok(formatter
            .default_status
            .unwrap_or(StatusCode::NO_CONTENT)
            .into_response());
    }

    let default_status = formatter.default_status.unwrap_or(StatusCode::OK);
    let mut final_result = result;
    let validated_storage;
    let defer_background = result.is_instance_of::<PyStreamingResponse>()
        || result.is_instance_of::<PyEventSourceResponse>()
        || matches!(
            response_kind(result),
            ResponseKind::Streaming | ResponseKind::Sse
        );
    if !defer_background {
        spawn_response_background(py, result);
    }

    if response_kind(final_result) == ResponseKind::File
        || (final_result.hasattr("path").unwrap_or(false)
            && final_result.hasattr("filename").unwrap_or(false))
    {
        return Ok(convert_file_response(py, final_result, request_headers));
    }

    if !final_result.is_instance_of::<PyFileResponse>()
        && !final_result.is_instance_of::<PyStreamingResponse>()
        && !final_result.is_instance_of::<PyEventSourceResponse>()
        && response_kind(final_result) != ResponseKind::File
        && response_kind(final_result) != ResponseKind::Streaming
        && response_kind(final_result) != ResponseKind::Sse
        && let Some(response) = rendered_response(final_result)
    {
        return Ok(response);
    }
    if final_result.is_instance_of::<PyStreamingResponse>()
        || final_result.is_instance_of::<PyEventSourceResponse>()
    {
        return Ok(convert_streaming_response(py, final_result));
    } else if final_result.is_instance_of::<PyFileResponse>() {
        return Ok(convert_file_response(py, final_result, request_headers));
    }

    let is_plain_value = final_result.is_instance_of::<PyDict>()
        || final_result.is_instance_of::<PyList>()
        || final_result.is_instance_of::<PyString>()
        || final_result.is_instance_of::<PyBool>()
        || final_result.is_instance_of::<PyInt>()
        || final_result.is_instance_of::<PyFloat>();

    let mut kind = if is_plain_value {
        ResponseKind::Unknown
    } else {
        let kind = response_kind(final_result);
        if kind == ResponseKind::Base {
            return Ok(convert_auto_response(py, final_result));
        }
        kind
    };

    if !is_plain_value && is_starlette_response(final_result) {
        return Ok(convert_auto_response(py, final_result));
    }

    if !is_plain_value {
        match kind {
            ResponseKind::Json => {
                return Ok(convert_json_response(py, final_result));
            }
            ResponseKind::Text => {
                return Ok(convert_text_response(py, final_result));
            }
            ResponseKind::Html => {
                return Ok(convert_html_response(py, final_result));
            }
            ResponseKind::Redirect => {
                return Ok(convert_redirect_response(py, final_result));
            }
            ResponseKind::Streaming | ResponseKind::Sse => {
                return Ok(convert_streaming_response(py, final_result));
            }
            _ => {}
        }
    }

    if let Some(model) = &formatter.response_model {
        validated_storage = validate_response_model(py, model.bind(py), final_result)?;
        final_result = &validated_storage;
        kind = response_kind(final_result);
    }

    let response = match formatter.response_type {
        ResponseType::PlainText => {
            let body_bytes = final_result
                .extract::<&str>()
                .map(|s| Bytes::copy_from_slice(s.as_bytes()))
                .or_else(|_| {
                    final_result.str().and_then(|py_str| {
                        let s: &str = py_str.to_str()?;
                        Ok(Bytes::copy_from_slice(s.as_bytes()))
                    })
                })
                .unwrap_or_else(|_| Bytes::new());

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

        ResponseType::Json => py_json_response_with_dump_options(
            py,
            default_status,
            final_result,
            formatter.serialization_hint,
            formatter.dump_options.as_ref().map(|d| d.bind(py)),
        )?,

        ResponseType::Html => convert_html_response(py, final_result),

        ResponseType::Redirect => convert_redirect_response(py, final_result),

        ResponseType::File => convert_file_response(py, final_result, request_headers),

        ResponseType::Auto => {
            if final_result.is_instance_of::<PyJSONResponse>() || kind == ResponseKind::Json {
                convert_json_response(py, final_result)
            } else if final_result.is_instance_of::<PyPlainTextResponse>()
                || kind == ResponseKind::Text
            {
                convert_text_response(py, final_result)
            } else if final_result.is_instance_of::<PyHTMLResponse>() || kind == ResponseKind::Html
            {
                convert_html_response(py, final_result)
            } else if final_result.is_instance_of::<PyRedirectResponse>()
                || kind == ResponseKind::Redirect
            {
                convert_redirect_response(py, final_result)
            } else if final_result.is_instance_of::<PyStreamingResponse>()
                || kind == ResponseKind::Streaming
                || final_result.is_instance_of::<PyEventSourceResponse>()
                || kind == ResponseKind::Sse
            {
                convert_streaming_response(py, final_result)
            } else if final_result.is_instance_of::<PyFileResponse>() || kind == ResponseKind::File
            {
                convert_file_response(py, final_result, request_headers)
            } else {
                py_to_response(py, final_result, default_status)
            }
        }
    };

    Ok(response)
}

#[inline(always)]
pub fn convert_html_response(py: Python, result: &Bound<PyAny>) -> Response {
    if let Some(response) = rendered_response(result) {
        return response;
    }
    if let Ok(resp_bound) = result.cast::<PyHTMLResponse>() {
        let resp = resp_bound.borrow();
        let status_code = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK);
        let res = (
            status_code,
            Html(
                resp.content
                    .bind(py)
                    .extract::<String>()
                    .unwrap_or_default(),
            ),
        )
            .into_response();
        apply_response_metadata(py, res, resp.headers.as_ref(), resp.media_type.as_deref())
    } else if response_kind(result) == ResponseKind::Html {
        let data = extract_base_response_data(result, StatusCode::OK);
        let content = data
            .content
            .and_then(|content| content.extract::<String>().ok())
            .unwrap_or_default();
        let res = (data.status_code, Html(content)).into_response();
        apply_response_metadata(py, res, data.headers.as_ref(), data.media_type.as_deref())
    } else {
        error!("Expected HTMLResponse, but got another type.");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

#[inline(always)]
pub fn convert_json_response(py: Python, result: &Bound<PyAny>) -> Response {
    if let Some(response) = rendered_response(result) {
        return response;
    }
    if let Ok(resp_bound) = result.cast::<PyJSONResponse>() {
        let resp = resp_bound.borrow();
        let status_code = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK);
        let res = py_json_response_with_status(py, status_code, resp.content.bind(py))
            .unwrap_or_else(|err| {
                err.print(py);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            });
        apply_response_metadata(py, res, resp.headers.as_ref(), resp.media_type.as_deref())
    } else if let Ok(resp_bound) = result.cast::<PyORJSONResponse>() {
        let resp = resp_bound.borrow();
        let status_code = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK);
        let res = py_json_response_with_status(py, status_code, resp.content.bind(py))
            .unwrap_or_else(|err| {
                err.print(py);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            });
        apply_response_metadata(py, res, resp.headers.as_ref(), resp.media_type.as_deref())
    } else if let Ok(resp_bound) = result.cast::<PyUJSONResponse>() {
        let resp = resp_bound.borrow();
        let status_code = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK);
        let res = py_json_response_with_status(py, status_code, resp.content.bind(py))
            .unwrap_or_else(|err| {
                err.print(py);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            });
        apply_response_metadata(py, res, resp.headers.as_ref(), resp.media_type.as_deref())
    } else {
        if response_kind(result) != ResponseKind::Json {
            error!("Expected JSONResponse, but got another type.");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }

        let data = extract_base_response_data(result, StatusCode::OK);
        let res = match data.content {
            Some(ref content) => py_json_response_with_status(py, data.status_code, content)
                .unwrap_or_else(|err| {
                    err.print(py);
                    StatusCode::INTERNAL_SERVER_ERROR.into_response()
                }),
            None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
        apply_response_metadata(py, res, data.headers.as_ref(), data.media_type.as_deref())
    }
}

#[inline(always)]
pub fn convert_text_response(py: Python, result: &Bound<PyAny>) -> Response {
    if let Some(response) = rendered_response(result) {
        return response;
    }
    if let Ok(resp_bound) = result.cast::<PyPlainTextResponse>() {
        let resp = resp_bound.borrow();
        let status_code = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK);
        let res = (
            status_code,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            )],
            resp.content
                .bind(py)
                .extract::<String>()
                .unwrap_or_default(),
        )
            .into_response();
        apply_response_metadata(py, res, resp.headers.as_ref(), resp.media_type.as_deref())
    } else if response_kind(result) == ResponseKind::Text {
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
        apply_response_metadata(py, res, data.headers.as_ref(), data.media_type.as_deref())
    } else {
        error!("Expected PlainTextResponse, but got another type.");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

#[inline(always)]
pub fn convert_redirect_response(py: Python, result: &Bound<PyAny>) -> Response {
    if let Some(response) = rendered_response(result) {
        return response;
    }
    if let Ok(resp_bound) = result.cast::<PyRedirectResponse>() {
        let resp = resp_bound.borrow();
        let res = if resp.status_code == 301 {
            Redirect::permanent(&resp.url).into_response()
        } else {
            Redirect::temporary(&resp.url).into_response()
        };
        apply_response_metadata(py, res, resp.headers.as_ref(), None)
    } else if response_kind(result) == ResponseKind::Redirect {
        let url = result
            .getattr(intern!(py, "url"))
            .ok()
            .and_then(|url| url.extract::<String>().ok())
            .unwrap_or_default();
        let status = result
            .getattr(intern!(py, "status_code"))
            .ok()
            .and_then(|status| status.extract::<u16>().ok())
            .unwrap_or(307);
        let headers = result
            .getattr(intern!(py, "headers"))
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
fn sse_chunk(py: Python<'_>, value: &Bound<'_, PyAny>, sep: &str) -> bytes::Bytes {
    if let Some((data, raw_data, event, id, retry, comment)) = event_fields(py, value) {
        return format_sse_event(
            py,
            data.as_ref(),
            raw_data.as_deref(),
            event.as_deref(),
            id.as_deref(),
            retry,
            comment.as_deref(),
            sep,
        );
    }
    format_sse_event(py, Some(value), None, None, None, None, None, sep)
}

pub fn convert_streaming_response(py: Python, result: &Bound<PyAny>) -> Response {
    let background = result
        .getattr(intern!(py, "background"))
        .ok()
        .filter(|value| !value.is_none())
        .map(Bound::unbind);
    let (content, status_code, headers, media_type, sse, ping, sep) =
        if let Ok(resp_bound) = result.cast::<PyStreamingResponse>() {
            let resp = resp_bound.borrow();
            (
                resp.content.clone_ref(py),
                StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK),
                resp.headers.clone(),
                resp.media_type.clone(),
                false,
                None,
                "\n".to_owned(),
            )
        } else if let Ok(resp_bound) = result.cast::<PyEventSourceResponse>() {
            let resp = resp_bound.borrow();
            (
                resp.content.clone_ref(py),
                StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK),
                resp.headers.clone(),
                resp.media_type.clone(),
                true,
                sse_options(result).0,
                sse_options(result).1,
            )
        } else if matches!(
            response_kind(result),
            ResponseKind::Streaming | ResponseKind::Sse
        ) {
            let data = extract_base_response_data(result, StatusCode::OK);
            let content = data.content.map_or_else(|| py.None(), Bound::unbind);
            (
                content,
                data.status_code,
                data.headers,
                data.media_type,
                response_kind(result) == ResponseKind::Sse,
                sse_options(result).0,
                sse_options(result).1,
            )
        } else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };

    let request_loop = py_bridge::request_loop().or_else(server_loop);
    let Some(request_loop) = request_loop else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let ping_chunk =
        sse.then(|| format_sse_event(py, None, None, None, None, None, Some("ping"), &sep));

    let stream = async_stream::stream! {
        let (is_async, is_sync) = Python::attach(|py| {
            let b = content.bind(py);
            (b.hasattr(intern!(py, "__anext__")).unwrap_or(false), b.hasattr(intern!(py, "__next__")).unwrap_or(false))
        });

        if is_async {
            loop {
                let fut = Python::attach(|py| {
                    let awaitable = match content.bind(py).call_method0(intern!(py, "__anext__")) {
                        Ok(a) => Some(a),
                        Err(e) => {
                            if !e.is_instance_of::<PyStopAsyncIteration>(py)
                                && !e.is_instance_of::<PyStopIteration>(py)
                            {
                                error!("StreamingResponse async error: {:?}", e);
                            }
                            None
                        }
                    };
                    awaitable.and_then(|a| {
                        let locals = TaskLocals::new(request_loop.bind(py).clone());
                        py_bridge::into_future_with_locals(&locals, a).ok()
                    })
                });
                let Some(fut) = fut else { break; };
                let result = if let Some(ping) = ping {
                    if let Ok(result) = timeout(Duration::from_secs_f64(ping), fut).await { result } else {
                        if let Some(chunk) = ping_chunk.clone() {
                            yield Ok::<_, Infallible>(chunk);
                        }
                        continue;
                    }
                } else {
                    fut.await
                };
                match result {
                    Ok(val) => {
                        let bytes = Python::attach(|py| {
                            let bound = val.bind(py);
                            if sse { Ok(sse_chunk(py, bound, &sep)) }
                            else if let Ok(s) = bound.extract::<&str>() { Ok(Bytes::copy_from_slice(s.as_bytes())) }
                            else if let Ok(b) = bound.extract::<&[u8]>() { Ok(Bytes::copy_from_slice(b)) }
                            else { Err(()) }
                        });
                        if let Ok(b) = bytes { yield Ok::<_, Infallible>(b); }
                    }
                    Err(e) => {
                        Python::attach(|py| if !e.is_instance_of::<PyStopAsyncIteration>(py) { error!("StreamingResponse async error: {:?}", e); });
                        break;
                    }
                }
            }
        } else if is_sync {
            loop {
                let chunk = Python::attach(|py| {
                    match content.bind(py).call_method0(intern!(py, "__next__")) {
                        Ok(val) => {
                            if sse { Ok(Some(sse_chunk(py, &val, &sep))) }
                            else if let Ok(s) = val.extract::<&str>() { Ok(Some(Bytes::copy_from_slice(s.as_bytes()))) }
                            else if let Ok(b) = val.extract::<&[u8]>() { Ok(Some(Bytes::copy_from_slice(b))) }
                            else { Err(()) }
                        }
                        Err(e) => {
                            if !e.is_instance_of::<PyStopIteration>(py) {
                                error!("StreamingResponse sync error: {:?}", e);
                            }
                            Ok(None)
                        }
                    }
                });
                match chunk {
                    Ok(Some(b)) => yield Ok::<_, Infallible>(b),
                    Ok(None) => break,
                    Err(()) => {}
                }
            }
        }
        if let Some(background) = background {
            spawn_response_background_value(background);
        }
    };
    let res = (status_code, Body::from_stream(stream)).into_response();
    let media_type = media_type.or_else(|| sse.then_some("text/event-stream".to_owned()));
    apply_response_metadata(py, res, headers.as_ref(), media_type.as_deref())
}

#[inline(always)]
#[must_use]
pub fn convert_auto_response(py: Python, result: &Bound<PyAny>) -> Response {
    if let Some(response) = rendered_response(result) {
        return response;
    }
    if result.is_none() {
        return StatusCode::NO_CONTENT.into_response();
    }

    let kind = response_kind(result);
    if result.is_instance_of::<PyJSONResponse>() || kind == ResponseKind::Json {
        return convert_json_response(py, result);
    }
    if result.is_instance_of::<PyPlainTextResponse>() || kind == ResponseKind::Text {
        return convert_text_response(py, result);
    }
    if result.is_instance_of::<PyHTMLResponse>() || kind == ResponseKind::Html {
        return convert_html_response(py, result);
    }
    if result.is_instance_of::<PyRedirectResponse>() || kind == ResponseKind::Redirect {
        return convert_redirect_response(py, result);
    }
    if result.is_instance_of::<PyStreamingResponse>() || kind == ResponseKind::Streaming {
        return convert_streaming_response(py, result);
    }
    if result.is_instance_of::<PyEventSourceResponse>() || kind == ResponseKind::Sse {
        return convert_streaming_response(py, result);
    }
    if result.is_instance_of::<PyFileResponse>() || kind == ResponseKind::File {
        return convert_file_response(py, result, None);
    }

    let status = response_status(result, StatusCode::OK);
    py_json_response_with_status(py, status, result).unwrap_or_else(|err| {
        err.print(py);
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })
}
