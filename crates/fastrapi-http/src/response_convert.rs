//! This module converts Python return values into axum responses.
//! It supports the fastrapi response classes, starlette responses, and plain Python values.

use super::response_file::convert_file_response;
use super::responses::{
    PyFileResponse, PyRedirectResponse, ResponseKind, extract_base_response_data, response_kind,
    response_status,
};
use crate::background::{
    BackgroundJob, BackgroundRun, OwnBackground, run_after_body, spawn_response_background_value,
};
use crate::exceptions::PyFastAPIError;
use crate::exceptions::server_error_response;
use crate::response_api::content_type;
use crate::response_type::{ResponseFormatter, ResponseType};
use crate::responses::{PyHTMLResponse, PyJSONResponse, PyPlainTextResponse, PyStreamingResponse};
use crate::sse::PyEventSourceResponse;
use crate::sse::{event_fields, format_sse_event, json_dumps, sse_options};
use crate::stream::StreamItems;
use axum::body::Body;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use bytes::Bytes;
use fastrapi_pyutil::body::{bytes_body, str_body};
use fastrapi_pyutil::json::{
    py_json_response_with_dump_options, py_json_response_with_status, py_to_response,
};
use fastrapi_runtime::blocking::run_python;
use fastrapi_runtime::py_bridge;
use fastrapi_runtime::py_bridge::EventLoop;
use fastrapi_runtime::server_state::{current_server, server_loop};
use fastrapi_runtime::tokio_runtime::spawn;
use pyo3::exceptions::PyValueError;
use pyo3::exceptions::{PyRuntimeError, PyStopAsyncIteration, PyStopIteration};
use pyo3::prelude::*;
use pyo3::types::{
    PyBool, PyByteArray, PyBytes, PyDict, PyFloat, PyInt, PyList, PyMemoryView, PyString, PyTuple,
};
use pyo3::{Bound, Py, PyAny, intern};
use pyo3_async_runtimes::TaskLocals;
use std::collections::HashSet;
use tokio::time::timeout;
use tracing::error;

pub fn apply_response_metadata(
    py: Python<'_>,
    mut res: Response,
    headers: Option<&Py<PyDict>>,
    media_type: Option<&str>,
) -> Response {
    // Like Starlette's `init_headers`: supplied headers replace the defaults,
    // and a supplied `content-type` wins over `media_type`.
    let mut supplied: Vec<HeaderName> = Vec::new();
    if let Some(headers) = headers {
        for (key, value) in headers.bind(py).iter() {
            let parsed = key
                .extract::<&str>()
                .ok()
                .zip(value.extract::<&str>().ok())
                .and_then(|(key, value)| {
                    Some((
                        HeaderName::try_from(key).ok()?,
                        HeaderValue::try_from(value).ok()?,
                    ))
                });
            let Some((name, value)) = parsed else {
                error!("the response has an invalid header");
                return render_failed(py, &PyValueError::new_err("Invalid HTTP header value."));
            };
            if supplied.contains(&name) {
                res.headers_mut().append(name, value);
            } else {
                res.headers_mut().insert(name.clone(), value);
                supplied.push(name);
            }
        }
    }

    if let Some(media_type) = media_type
        && !supplied.contains(&CONTENT_TYPE)
        && let Ok(hval) = HeaderValue::try_from(media_type)
    {
        res.headers_mut().insert(CONTENT_TYPE, hval);
    }

    res
}

/// A response that could not be rendered fails like an exception raised in the endpoint.
fn render_failed(py: Python<'_>, err: &PyErr) -> Response {
    error!("failed to render the response: {err}");
    server_error_response(py, err, None)
}

/// Encodes response content the way Starlette's `Response.render` does.
fn render_body(py: Python<'_>, content: &Bound<'_, PyAny>, charset: &str) -> PyResult<Bytes> {
    let rendered = crate::response_api::render(py, content, "Response", charset)?;
    if let Ok(bytes) = rendered.cast::<PyBytes>() {
        return Ok(bytes_body(bytes));
    }
    let bytes = py.get_type::<PyBytes>().call1((rendered,))?;
    Ok(bytes_body(bytes.cast::<PyBytes>()?))
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
    let body = if let Ok(bytes) = body.cast::<PyBytes>() {
        bytes_body(bytes)
    } else if let Ok(bytes) = body.extract::<&[u8]>() {
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

pub use crate::stream::stream_factory;

pub(crate) fn validate_stream_item<'py>(
    py: Python<'py>,
    adapter: &Bound<'py, PyAny>,
    item: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    response_helpers(py)?
        .getattr(intern!(py, "_validate_response"))?
        .call1((adapter, item))
}

static SERIALIZE_RESPONSE: pyo3::sync::PyOnceLock<Py<PyAny>> = pyo3::sync::PyOnceLock::new();
static VALIDATE_RESPONSE: pyo3::sync::PyOnceLock<Py<PyAny>> = pyo3::sync::PyOnceLock::new();

fn response_helper<'py>(
    py: Python<'py>,
    cell: &'static pyo3::sync::PyOnceLock<Py<PyAny>>,
    name: &Bound<'py, PyString>,
) -> PyResult<&'static Py<PyAny>> {
    cell.get_or_try_init(py, || {
        response_helpers(py)?.getattr(name).map(Bound::unbind)
    })
}

fastrapi_pyutil::cached_py_import!(TYPE_ADAPTER, "pydantic", "TypeAdapter");
fastrapi_pyutil::cached_py_import!(
    SCHEMA_GENERATION_ERROR,
    "pydantic.errors",
    "PydanticSchemaGenerationError"
);

/// The `TypeAdapter` of a response model. Like FastAPI's `create_model_field`, a type
/// pydantic can't build a schema for raises `FastAPIError`.
pub fn response_type_adapter<'py>(model: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = model.py();
    let kwargs = PyDict::new(py);
    kwargs.set_item(intern!(py, "_parent_depth"), 0)?;
    TYPE_ADAPTER
        .get(py)?
        .call((model,), Some(&kwargs))
        .map_err(|err| {
            if !SCHEMA_GENERATION_ERROR
                .get(py)
                .is_ok_and(|class| err.is_instance(py, class))
            {
                return err;
            }
            let type_name = model.str().map_or_else(|_| "?".into(), |name| name.to_string());
            PyErr::from_type(
                py.get_type::<PyFastAPIError>(),
                format!(
                    "Invalid args for response field! Hint: check that {type_name} is a valid Pydantic field type. If you are using a return type annotation that is not a valid Pydantic field (e.g. Union[Response, dict, None]) you can disable generating the response model from the type annotation with the path operation decorator parameter response_model=None. Read more: https://fastapi.tiangolo.com/tutorial/response-model/"
                ),
            )
        })
}

fn response_adapter<'a>(
    py: Python<'_>,
    formatter: &'a ResponseFormatter,
    model: &Py<PyAny>,
) -> PyResult<&'a Py<PyAny>> {
    formatter.response_adapter.get_or_try_init(py, || {
        response_type_adapter(model.bind(py)).map(Bound::unbind)
    })
}

/// Builds the route's response model adapter when the route is defined, as FastAPI does.
pub fn prepare_response_model(py: Python<'_>, formatter: &ResponseFormatter) -> PyResult<()> {
    match formatter
        .response_model
        .as_ref()
        .filter(|model| !model.bind(py).is_none())
    {
        Some(model) => response_adapter(py, formatter, model).map(drop),
        None => Ok(()),
    }
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
    Ok(bytes_body(json.cast::<PyBytes>()?))
}

const fn body_allowed(status: StatusCode) -> bool {
    crate::status::body_allowed(status.as_u16())
}

fn null_json_response(status: StatusCode) -> Response {
    (
        status,
        [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
        Bytes::from_static(b"null"),
    )
        .into_response()
}

#[must_use]
pub fn is_plain_value(value: &Bound<'_, PyAny>) -> bool {
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
        if !matches!(kind, ResponseKind::Streaming | ResponseKind::Sse)
            && let Some(event_loop) = current_server()
        {
            response = run_after_body(
                response,
                BackgroundRun(BackgroundJob::Value(background.unbind()), event_loop),
            );
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
        match formatter.response_type {
            ResponseType::Json | ResponseType::Auto if body_allowed(status) => {
                return Ok(null_json_response(status));
            }
            ResponseType::Custom => {}
            _ => return Ok(status.into_response()),
        }
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
            let body_bytes = if let Ok(bytes) = final_result.cast::<PyBytes>() {
                bytes_body(bytes)
            } else {
                final_result
                    .cast::<PyString>()
                    .map_err(PyErr::from)
                    .and_then(str_body)
                    .or_else(|_| final_result.str().and_then(|py_str| str_body(&py_str)))?
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

        ResponseType::Json => py_json_response_with_dump_options(
            py,
            default_status,
            final_result,
            formatter.serialization_hint,
            formatter.dump_options.as_ref().map(|d| d.bind(py)),
        )?,

        ResponseType::Html if final_result.is_instance_of::<PyString>() => (
            default_status,
            Html(str_body(final_result.cast::<PyString>()?)?),
        )
            .into_response(),

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

        ResponseType::Custom => match &formatter.response_class {
            Some(class) => {
                custom_class_response(py, class, final_result, formatter.default_status)?
            }
            None => py_to_response(py, final_result, default_status),
        },

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

/// Builds the route's `response_class` from a plain return value, like FastAPI's
/// `response_class(content, status_code=...)`.
fn custom_class_response(
    py: Python<'_>,
    class: &Py<PyAny>,
    content: &Bound<'_, PyAny>,
    status: Option<StatusCode>,
) -> PyResult<Response> {
    let kwargs = PyDict::new(py);
    if let Some(status) = status {
        kwargs.set_item(intern!(py, "status_code"), status.as_u16())?;
    }
    let response = class.bind(py).call((content,), Some(&kwargs))?;
    Ok(convert_auto_response(py, &response))
}

#[inline(always)]
pub fn convert_html_response(py: Python, result: &Bound<PyAny>) -> Response {
    if let Some(response) = rendered_response(result) {
        return response;
    }
    if let Ok(resp_bound) = result.cast::<PyHTMLResponse>() {
        let resp = resp_bound.borrow();
        let status_code = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK);
        let content = match render_body(py, resp.content.bind(py), &resp.charset) {
            Ok(content) => content,
            Err(err) => return render_failed(py, &err),
        };
        let res = (status_code, Html(content)).into_response();
        apply_response_metadata(py, res, resp.headers.as_ref(), resp.media_type.as_deref())
    } else if response_kind(result) == ResponseKind::Html {
        let data = extract_base_response_data(result, StatusCode::OK);
        let content = match data.content.map_or_else(
            || Ok(Bytes::new()),
            |content| render_body(py, &content, "utf-8"),
        ) {
            Ok(content) => content,
            Err(err) => return render_failed(py, &err),
        };
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
            .unwrap_or_else(|err| render_failed(py, &err));
        apply_response_metadata(py, res, resp.headers.as_ref(), resp.media_type.as_deref())
    } else {
        if response_kind(result) != ResponseKind::Json {
            error!("Expected JSONResponse, but got another type.");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }

        let data = extract_base_response_data(result, StatusCode::OK);
        let res = match data.content {
            Some(ref content) => py_json_response_with_status(py, data.status_code, content)
                .unwrap_or_else(|err| render_failed(py, &err)),
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
        let content = match render_body(py, resp.content.bind(py), &resp.charset) {
            Ok(content) => content,
            Err(err) => return render_failed(py, &err),
        };
        let res = (
            status_code,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            )],
            content,
        )
            .into_response();
        apply_response_metadata(py, res, resp.headers.as_ref(), resp.media_type.as_deref())
    } else if response_kind(result) == ResponseKind::Text {
        let data = extract_base_response_data(result, StatusCode::OK);
        let content = match data.content.map_or_else(
            || Ok(Bytes::new()),
            |content| render_body(py, &content, "utf-8"),
        ) {
            Ok(content) => content,
            Err(err) => return render_failed(py, &err),
        };
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

pub(crate) fn sse_event_chunk(
    py: Python<'_>,
    value: &Bound<'_, PyAny>,
    sep: &str,
) -> PyResult<bytes::Bytes> {
    let Some((data, raw_data, event, id, retry, comment)) = event_fields(py, value) else {
        return Ok(Bytes::new());
    };
    let data = match (raw_data, data) {
        (Some(raw), _) => Some(raw),
        (None, Some(data)) if data.hasattr(intern!(py, "model_dump_json"))? => Some(
            data.call_method0(intern!(py, "model_dump_json"))?
                .extract::<String>()?,
        ),
        (None, Some(data)) => Some(json_dumps(&data)?),
        (None, None) => None,
    };
    Ok(format_sse_event(
        data.as_deref(),
        event.as_deref(),
        id.as_deref(),
        retry,
        comment.as_deref(),
        sep,
    ))
}

#[inline(always)]
fn sse_chunk(py: Python<'_>, value: &Bound<'_, PyAny>, sep: &str) -> PyResult<bytes::Bytes> {
    if event_fields(py, value).is_some() {
        return sse_event_chunk(py, value, sep);
    }
    let mut data = Vec::new();
    fastrapi_pyutil::json::write_py_json(py, value, &mut data)?;
    Ok(format_sse_event(
        Some(&String::from_utf8_lossy(&data)),
        None,
        None,
        None,
        None,
        sep,
    ))
}

fn stream_chunk(py: Python<'_>, value: &Bound<'_, PyAny>, sse: bool, sep: &str) -> PyResult<Bytes> {
    if sse {
        return sse_chunk(py, value, sep);
    }
    if let Ok(bytes) = value.cast::<PyBytes>() {
        return Ok(bytes_body(bytes));
    }
    if value.is_instance_of::<PyByteArray>() || value.is_instance_of::<PyMemoryView>() {
        let bytes = py.get_type::<PyBytes>().call1((value,))?;
        return Ok(bytes_body(bytes.cast::<PyBytes>()?));
    }
    if let Ok(text) = value.cast::<PyString>() {
        return str_body(text);
    }
    let encoded = value.call_method1(intern!(py, "encode"), ("utf-8",))?;
    Ok(bytes_body(encoded.cast::<PyBytes>()?))
}

/// Cleans up a streaming body when it ends or is dropped early by a client disconnect.
/// Like Starlette's `StreamingResponse`, the background task still runs after a disconnect,
/// but not after the stream raised. An unfinished async iterator is `aclose()`d.
struct StreamGuard {
    background: Option<Py<PyAny>>,
    iterator: Option<Py<PyAny>>,
    event_loop: EventLoop,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        if let Some(background) = self.background.take() {
            spawn_response_background_value(background);
        }
        let Some(iterator) = self.iterator.take() else {
            return;
        };
        let event_loop = self.event_loop;
        Python::attach(|py| {
            let closing = iterator
                .bind(py)
                .call_method0(intern!(py, "aclose"))
                .and_then(|closing| py_bridge::schedule_task(py, event_loop, closing));
            if let Ok(closing) = closing {
                // An `__anext__` still running makes `aclose()` raise; asyncio's
                // async-generator finalizer closes it once that call finishes.
                spawn(async move {
                    _ = closing.await;
                });
            }
        });
    }
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
            let sse = result.is_instance_of::<PyEventSourceResponse>();
            let (ping, sep) = if sse {
                sse_options(result)
            } else {
                (None, "\n".to_owned())
            };
            (
                resp.content.clone_ref(py),
                StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK),
                resp.headers.clone(),
                resp.media_type
                    .as_deref()
                    .map(|media_type| content_type(media_type, &resp.charset).into_owned()),
                sse,
                ping,
                sep,
            )
        } else if matches!(
            response_kind(result),
            ResponseKind::Streaming | ResponseKind::Sse
        ) {
            let data = extract_base_response_data(result, StatusCode::OK);
            let content = data.content.map_or_else(|| py.None(), Bound::unbind);
            let (ping, sep) = sse_options(result);
            (
                content,
                data.status_code,
                data.headers,
                data.media_type
                    .as_deref()
                    .map(|media_type| content_type(media_type, "utf-8").into_owned()),
                response_kind(result) == ResponseKind::Sse,
                ping,
                sep,
            )
        } else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };

    let request_loop = py_bridge::request_loop().or_else(server_loop);
    let Some(request_loop) = request_loop else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let ping_chunk = sse.then(|| format_sse_event(None, None, None, None, Some("ping"), &sep));

    let (content, items) = match content.bind(py).cast::<StreamItems>() {
        Ok(items) => (
            items.get().content.clone_ref(py),
            Some(items.clone().unbind()),
        ),
        Err(_) => (content, None),
    };
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
    let is_async = content
        .bind(py)
        .hasattr(intern!(py, "__anext__"))
        .unwrap_or(false);
    let locals = TaskLocals::new(request_loop.bind(py).clone());
    let mut guard = StreamGuard {
        background,
        iterator: is_async.then(|| content.clone_ref(py)),
        event_loop: request_loop,
    };

    let stream = async_stream::stream! {
        loop {
            let next: PyResult<Option<Py<PyAny>>> = if is_async {
                let pending = Python::attach(|py| -> PyResult<Option<_>> {
                    match content.bind(py).call_method0(intern!(py, "__anext__")) {
                        Ok(awaitable) => {
                            py_bridge::into_future_with_locals(&locals, awaitable).map(Some)
                        }
                        Err(error) if error.is_instance_of::<PyStopAsyncIteration>(py) => Ok(None),
                        Err(error) => Err(error),
                    }
                });
                match pending {
                    Ok(Some(future)) => {
                        let awaited = if let Some(period) = ping {
                            let mut future = std::pin::pin!(future);
                            loop {
                                if let Ok(result) = timeout(period, future.as_mut()).await {
                                    break result;
                                }
                                if let Some(chunk) = ping_chunk.clone() {
                                    yield Ok::<_, std::io::Error>(chunk);
                                }
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
                Ok(Some(value)) => Python::attach(|py| match &items {
                    Some(items) => items.get().encode(py, value.bind(py), &sep),
                    None => stream_chunk(py, value.bind(py), sse, &sep),
                }),
                Ok(None) => {
                    guard.iterator = None;
                    break;
                }
                Err(error) => Err(error),
            };
            match chunk {
                Ok(bytes) => yield Ok(bytes),
                Err(error) => {
                    error!("StreamingResponse failed");
                    Python::attach(|py| error.print(py));
                    guard.background = None;
                    yield Err(std::io::Error::other("streaming response failed"));
                    break;
                }
            }
        }
        drop(guard);
    };
    let res = (status_code, Body::from_stream(stream)).into_response();
    let media_type = media_type
        .as_deref()
        .or_else(|| sse.then_some("text/event-stream; charset=utf-8"));
    apply_response_metadata(py, res, headers.as_ref(), media_type)
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
    if result.is_instance_of::<PyStreamingResponse>()
        || matches!(kind, ResponseKind::Streaming | ResponseKind::Sse)
    {
        return convert_streaming_response(py, result);
    }
    if result.is_instance_of::<PyFileResponse>() || kind == ResponseKind::File {
        return convert_file_response(py, result, None);
    }

    let status = response_status(result, StatusCode::OK);
    py_json_response_with_status(py, status, result).unwrap_or_else(|err| render_failed(py, &err))
}
