//! This module converts Python return values into axum responses.
//! It supports the fastrapi response classes, starlette responses, and plain Python values.

use super::response_file::convert_file_response;
use super::responses::{
    PyFileResponse, PyRedirectResponse, ResponseKind, extract_base_response_data, response_kind,
    response_status,
};
use crate::background::{OwnBackground, spawn_response_background_value};
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
use axum::response::{Html, IntoResponse, Response};
use bytes::Bytes;
use fastrapi_pyutil::json::{
    py_json_response_with_dump_options, py_json_response_with_status, py_to_response,
};
use fastrapi_runtime::blocking::run_python;
use fastrapi_runtime::py_bridge;
use fastrapi_runtime::server_state::server_loop;
use pyo3::exceptions::{PyRuntimeError, PyStopAsyncIteration, PyStopIteration};
use pyo3::prelude::*;
use pyo3::types::{
    PyBool, PyByteArray, PyBytes, PyDict, PyFloat, PyInt, PyList, PyMemoryView, PyString, PyTuple,
};
use pyo3::{Bound, Py, PyAny, intern};
use pyo3_async_runtimes::TaskLocals;
use std::collections::HashSet;
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
    let Ok(raw) = result.getattr(intern!(result.py(), "raw_headers")) else {
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
    let py = result.py();
    let body = result
        .getattr_opt(intern!(py, "body"))
        .ok()
        .flatten()
        .filter(|body| !body.is_none())?;
    let body = if let Ok(bytes) = body.extract::<&[u8]>() {
        Bytes::copy_from_slice(bytes)
    } else {
        let bytes = body.call_method0(intern!(py, "tobytes")).ok()?;
        Bytes::copy_from_slice(bytes.extract::<&[u8]>().ok()?)
    };
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = response_status(result, StatusCode::OK);
    Some(apply_raw_headers(response, result, RawHeaders::All))
}

const RESPONSE_HELPERS: &str = include_str!("py/responses.py");

static RESPONSE_HELPERS_MODULE: pyo3::sync::PyOnceLock<Py<pyo3::types::PyModule>> =
    pyo3::sync::PyOnceLock::new();

fn response_helpers(py: Python<'_>) -> PyResult<&Bound<'_, pyo3::types::PyModule>> {
    RESPONSE_HELPERS_MODULE
        .get_or_try_init(py, || {
            fastrapi_pyutil::imports::python_source_module(
                py,
                RESPONSE_HELPERS,
                c"fastrapi/_responses.py",
                c"fastrapi._responses",
            )
            .map(Bound::unbind)
        })
        .map(|module| module.bind(py))
}

pub fn stream_factory(
    py: Python<'_>,
    mode: &str,
    item_type: Option<&Bound<'_, PyAny>>,
    response_class: Option<&Bound<'_, PyAny>>,
    options: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyAny>> {
    Ok(response_helpers(py)?
        .getattr(intern!(py, "stream_factory"))?
        .call1((mode, item_type, response_class, options))?
        .unbind())
}

static SERIALIZE_RESPONSE: pyo3::sync::PyOnceLock<Py<PyAny>> = pyo3::sync::PyOnceLock::new();
static VALIDATE_RESPONSE: pyo3::sync::PyOnceLock<Py<PyAny>> = pyo3::sync::PyOnceLock::new();

fn response_helper<'py>(
    py: Python<'py>,
    cell: &'static pyo3::sync::PyOnceLock<Py<PyAny>>,
    name: &Bound<'py, PyString>,
) -> PyResult<&'static Py<PyAny>> {
    cell.get_or_try_init(py, || response_helpers(py)?.getattr(name).map(Bound::unbind))
}

fn response_adapter<'a>(
    py: Python<'_>,
    formatter: &'a ResponseFormatter,
    model: &Py<PyAny>,
) -> PyResult<&'a Py<PyAny>> {
    formatter.response_adapter.get_or_try_init(py, || {
        let kwargs = PyDict::new(py);
        kwargs.set_item(intern!(py, "_parent_depth"), 0)?;
        py.import(intern!(py, "pydantic"))?
            .getattr(intern!(py, "TypeAdapter"))?
            .call((model.bind(py),), Some(&kwargs))
            .map(Bound::unbind)
    })
}

fn apply_response_helper<'py>(
    py: Python<'py>,
    cell: &'static pyo3::sync::PyOnceLock<Py<PyAny>>,
    name: &Bound<'py, PyString>,
    formatter: &ResponseFormatter,
    model: &Py<PyAny>,
    content: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let adapter = response_adapter(py, formatter, model)?;
    response_helper(py, cell, name)?.bind(py).call(
        (adapter.bind(py), content),
        formatter
            .dump_options
            .as_ref()
            .map(|options| options.bind(py)),
    )
}

fn serialize_response_model(
    py: Python<'_>,
    formatter: &ResponseFormatter,
    model: &Py<PyAny>,
    content: &Bound<'_, PyAny>,
) -> PyResult<Bytes> {
    let json = apply_response_helper(
        py,
        &SERIALIZE_RESPONSE,
        intern!(py, "serialize_response"),
        formatter,
        model,
        content,
    )?;
    Ok(Bytes::copy_from_slice(
        json.cast::<pyo3::types::PyBytes>()?.as_bytes(),
    ))
}

const fn body_allowed(status: StatusCode) -> bool {
    !matches!(status.as_u16(), 100..=199 | 204 | 304)
}

fn null_json_response(status: StatusCode) -> Response {
    (
        status,
        [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
        Bytes::from_static(b"null"),
    )
        .into_response()
}

fn is_plain_value(value: &Bound<'_, PyAny>) -> bool {
    value.is_exact_instance_of::<PyDict>()
        || value.is_exact_instance_of::<PyList>()
        || value.is_exact_instance_of::<PyString>()
        || value.is_exact_instance_of::<PyInt>()
        || value.is_exact_instance_of::<PyBool>()
        || value.is_exact_instance_of::<PyFloat>()
}

#[inline(always)]
pub fn convert_response_by_type(
    py: Python,
    result: &Bound<PyAny>,
    formatter: &ResponseFormatter,
    request_headers: Option<&HeaderMap>,
) -> PyResult<Response> {
    if result.is_none() || is_plain_value(result) {
        return convert_response_value(py, result, formatter, request_headers);
    }
    let kind = response_kind(result);
    if kind == ResponseKind::Unknown {
        return convert_response_value(py, result, formatter, request_headers);
    }
    let background = result
        .getattr_opt(intern!(py, "background"))
        .ok()
        .flatten()
        .filter(|background| !background.is_none());
    let mut response = convert_response_object(py, result, kind, request_headers);
    if let Some(background) = background {
        if !matches!(kind, ResponseKind::Streaming | ResponseKind::Sse) {
            spawn_response_background_value(background.unbind());
        }
        response.extensions_mut().insert(OwnBackground);
    }
    Ok(response)
}

fn convert_response_object(
    py: Python,
    result: &Bound<PyAny>,
    kind: ResponseKind,
    request_headers: Option<&HeaderMap>,
) -> Response {
    match kind {
        ResponseKind::File => convert_file_response(py, result, request_headers),
        ResponseKind::Streaming | ResponseKind::Sse => convert_streaming_response(py, result),
        ResponseKind::Json => convert_json_response(py, result),
        ResponseKind::Text => convert_text_response(py, result),
        ResponseKind::Html => convert_html_response(py, result),
        ResponseKind::Redirect => convert_redirect_response(py, result),
        ResponseKind::Base | ResponseKind::Unknown => convert_auto_response(py, result),
    }
}

#[inline(always)]
fn convert_response_value(
    py: Python,
    result: &Bound<PyAny>,
    formatter: &ResponseFormatter,
    request_headers: Option<&HeaderMap>,
) -> PyResult<Response> {
    if let Some(factory) = &formatter.stream_factory
        && !is_plain_value(result)
        && response_kind(result) == ResponseKind::Unknown
        && (result.hasattr(intern!(py, "__anext__"))? || result.hasattr(intern!(py, "__next__"))?)
    {
        let streamed = factory.bind(py).call1((result,))?;
        return convert_response_by_type(py, &streamed, formatter, request_headers);
    }
    if result.is_none() {
        let status = formatter.default_status.unwrap_or(StatusCode::OK);
        return Ok(
            if matches!(
                formatter.response_type,
                ResponseType::Json | ResponseType::Auto
            ) && body_allowed(status)
            {
                null_json_response(status)
            } else {
                status.into_response()
            },
        );
    }

    let default_status = formatter.default_status.unwrap_or(StatusCode::OK);
    let mut final_result = result;
    let validated_storage;
    let mut kind = ResponseKind::Unknown;

    if let Some(model) = formatter
        .response_model
        .as_ref()
        .filter(|model| !model.bind(py).is_none())
    {
        if matches!(
            formatter.response_type,
            ResponseType::Json | ResponseType::Auto
        ) {
            let body = serialize_response_model(py, formatter, model, final_result)?;
            let response = if body_allowed(default_status) {
                (
                    default_status,
                    [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
                    body,
                )
                    .into_response()
            } else {
                default_status.into_response()
            };
            return Ok(response);
        }
        validated_storage = apply_response_helper(
            py,
            &VALIDATE_RESPONSE,
            intern!(py, "validate_response"),
            formatter,
            model,
            final_result,
        )?;
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

        ResponseType::Html if final_result.is_instance_of::<PyString>() => {
            (default_status, Html(final_result.extract::<String>()?)).into_response()
        }

        ResponseType::Html => convert_html_response(py, final_result),

        ResponseType::Redirect if final_result.is_instance_of::<PyString>() => {
            let location = HeaderValue::from_str(final_result.extract::<&str>()?)
                .map_err(|err| pyo3::exceptions::PyValueError::new_err(err.to_string()))?;
            (
                formatter
                    .default_status
                    .unwrap_or(StatusCode::TEMPORARY_REDIRECT),
                [(header::LOCATION, location)],
            )
                .into_response()
        }

        ResponseType::Redirect => convert_redirect_response(py, final_result),

        ResponseType::File
            if final_result.is_instance_of::<PyString>()
                || final_result.hasattr(intern!(py, "__fspath__"))? =>
        {
            let file = py.get_type::<PyFileResponse>().call1((final_result,))?;
            convert_file_response(py, &file, request_headers)
        }

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

    if body_allowed(default_status) {
        return Ok(response);
    }
    let (mut parts, _) = response.into_parts();
    parts.headers.remove(header::CONTENT_LENGTH);
    Ok(Response::from_parts(parts, Body::empty()))
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

fn redirect_response(status: u16, url: &str) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::TEMPORARY_REDIRECT);
    HeaderValue::try_from(url).map_or_else(
        |_| StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        |location| (status, [(header::LOCATION, location)]).into_response(),
    )
}

#[inline(always)]
pub fn convert_redirect_response(py: Python, result: &Bound<PyAny>) -> Response {
    if let Some(response) = rendered_response(result) {
        return response;
    }
    if let Ok(resp_bound) = result.cast::<PyRedirectResponse>() {
        let resp = resp_bound.borrow();
        let res = redirect_response(resp.status_code, &resp.url);
        apply_response_metadata(py, res, resp.headers.as_ref(), None)
    } else if response_kind(result) == ResponseKind::Redirect {
        let url = result
            .getattr(intern!(py, "url"))
            .ok()
            .and_then(|url| url.str().ok().map(|url| url.to_string()))
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
        let res = redirect_response(status, &url);
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

fn stream_chunk(py: Python<'_>, value: &Bound<'_, PyAny>, sse: bool, sep: &str) -> PyResult<Bytes> {
    if sse {
        return Ok(sse_chunk(py, value, sep));
    }
    if let Ok(bytes) = value.cast::<PyBytes>() {
        return Ok(Bytes::copy_from_slice(bytes.as_bytes()));
    }
    if value.is_instance_of::<PyByteArray>() || value.is_instance_of::<PyMemoryView>() {
        let bytes = py.get_type::<PyBytes>().call1((value,))?;
        return Ok(Bytes::copy_from_slice(bytes.cast::<PyBytes>()?.as_bytes()));
    }
    let encoded = value.call_method1(intern!(py, "encode"), ("utf-8",))?;
    Ok(Bytes::copy_from_slice(
        encoded.cast::<PyBytes>()?.as_bytes(),
    ))
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

    let content = {
        let bound = content.bind(py);
        let is_iterator = bound.hasattr(intern!(py, "__anext__")).unwrap_or(false)
            || bound.hasattr(intern!(py, "__next__")).unwrap_or(false);
        let iterator = if is_iterator {
            None
        } else if bound.hasattr(intern!(py, "__aiter__")).unwrap_or(false) {
            bound.call_method0(intern!(py, "__aiter__")).ok()
        } else {
            bound.try_iter().ok().map(Bound::into_any)
        };
        iterator.map_or_else(|| bound.clone().unbind(), Bound::unbind)
    };

    let stream = async_stream::stream! {
        let is_async = Python::attach(|py| {
            content.bind(py).hasattr(intern!(py, "__anext__")).unwrap_or(false)
        });
        loop {
            let next: PyResult<Option<Py<PyAny>>> = if is_async {
                let pending = Python::attach(|py| -> PyResult<Option<_>> {
                    match content.bind(py).call_method0(intern!(py, "__anext__")) {
                        Ok(awaitable) => {
                            let locals = TaskLocals::new(request_loop.bind(py).clone());
                            py_bridge::into_future_with_locals(&locals, awaitable).map(Some)
                        }
                        Err(error) if error.is_instance_of::<PyStopAsyncIteration>(py) => Ok(None),
                        Err(error) => Err(error),
                    }
                });
                match pending {
                    Ok(Some(future)) => {
                        let awaited = if let Some(ping) = ping {
                            if let Ok(result) = timeout(Duration::from_secs_f64(ping), future).await {
                                result
                            } else {
                                if let Some(chunk) = ping_chunk.clone() {
                                    yield Ok::<_, std::io::Error>(chunk);
                                }
                                continue;
                            }
                        } else {
                            future.await
                        };
                        match awaited {
                            Ok(value) => Ok(Some(value)),
                            Err(error) => {
                                if Python::attach(|py| error.is_instance_of::<PyStopAsyncIteration>(py)) {
                                    Ok(None)
                                } else {
                                    Err(error)
                                }
                            }
                        }
                    }
                    Ok(None) => Ok(None),
                    Err(error) => Err(error),
                }
            } else {
                let iterator = Python::attach(|py| content.clone_ref(py));
                match run_python(move |py| -> PyResult<Option<Py<PyAny>>> {
                    match iterator.bind(py).call_method0(intern!(py, "__next__")) {
                        Ok(value) => Ok(Some(value.unbind())),
                        Err(error) if error.is_instance_of::<PyStopIteration>(py) => Ok(None),
                        Err(error) => Err(error),
                    }
                })
                .await
                {
                    Ok(result) => result,
                    Err(join_error) => Err(PyRuntimeError::new_err(join_error.to_string())),
                }
            };
            let chunk = match next {
                Ok(Some(value)) => Python::attach(|py| stream_chunk(py, value.bind(py), sse, &sep)),
                Ok(None) => break,
                Err(error) => Err(error),
            };
            match chunk {
                Ok(bytes) => yield Ok(bytes),
                Err(error) => {
                    error!("StreamingResponse failed");
                    Python::attach(|py| error.print(py));
                    yield Err(std::io::Error::other("streaming response failed"));
                    break;
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
        return null_json_response(StatusCode::OK);
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
