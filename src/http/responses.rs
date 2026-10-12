use crate::engine::background::spawn_response_background;
use crate::engine::background::spawn_response_background_value;
use crate::globals::async_loop_pool;
pub use crate::http::sse::PyEventSourceResponse;
use crate::routing::types::RouteHandler;
use crate::utils::py_json_response_with_dump_options;
use axum::http::header::CONTENT_TYPE;
use bytes::Bytes;
use papaya::HashMap;
use pyo3::Bound;
use pyo3::exceptions::PyStopAsyncIteration;
use pyo3::exceptions::PyStopIteration;
use pyo3::exceptions::PyTypeError;
use pyo3::types::PyBool;
use pyo3::types::PyBytes;
use pyo3::types::PyFloat;
use pyo3::types::PyInt;
use pyo3::types::PyString;
use pyo3::types::PyTuple;
use pyo3_async_runtimes::TaskLocals;
use std::collections::HashSet;
use std::convert::Infallible;
use std::fs;
use std::fs::Metadata;
use std::iter::once;
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;
use tokio::fs::File;
use tokio::time::timeout;
#[path = "response_api.rs"]
pub mod api;
use crate::http::sse::{event_fields, format_sse_event, sse_options};
use crate::runtime::py_bridge;
use crate::utils::{py_json_response_with_status, py_to_response};
use axum::{
    body::Body,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
};
use pyo3::{intern, prelude::*, types::PyDict, types::PyType};
use std::ops::RangeInclusive;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tracing::error;

use pyo3::types::PyList;
use pyo3::{Py, PyAny, pyclass, pymethods};

use crate::types::response::ResponseType;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResponseKind {
    Unknown,
    Base,
    Json,
    Text,
    Html,
    Redirect,
    Streaming,
    Sse,
    File,
}

static RESPONSE_KIND_CACHE: LazyLock<papaya::HashMap<usize, (Py<PyType>, ResponseKind)>> =
    LazyLock::new(|| HashMap::with_capacity(32));

#[inline]
fn response_kind(result: &Bound<'_, PyAny>) -> ResponseKind {
    let ty = result.get_type();
    let type_ptr = ty.as_ptr() as usize;

    let guard = RESPONSE_KIND_CACHE.guard();
    if let Some((_, kind)) = RESPONSE_KIND_CACHE.get(&type_ptr, &guard) {
        return *kind;
    }
    drop(guard);

    let kind = probe_response_kind(&ty);
    RESPONSE_KIND_CACHE
        .pin()
        .insert(type_ptr, (ty.unbind(), kind));
    kind
}

fn probe_response_kind(ty: &Bound<'_, PyType>) -> ResponseKind {
    let Ok(name) = ty.name() else {
        return ResponseKind::Unknown;
    };
    let Ok(name) = name.to_str() else {
        return ResponseKind::Unknown;
    };
    match name.rsplit('.').next().unwrap_or(name) {
        "Response" => ResponseKind::Base,
        "JSONResponse" | "ORJSONResponse" | "UJSONResponse" => ResponseKind::Json,
        "PlainTextResponse" => ResponseKind::Text,
        "HTMLResponse" => ResponseKind::Html,
        "RedirectResponse" => ResponseKind::Redirect,
        "StreamingResponse" => ResponseKind::Streaming,
        "EventSourceResponse" => ResponseKind::Sse,
        "FileResponse" => ResponseKind::File,
        _ => ResponseKind::Unknown,
    }
}

fn is_starlette_response(result: &Bound<'_, PyAny>) -> bool {
    let py = result.py();
    result.hasattr(intern!(py, "body")).unwrap_or(false)
        && result.hasattr(intern!(py, "status_code")).unwrap_or(false)
        && result.hasattr(intern!(py, "headers")).unwrap_or(false)
}

fn response_status(result: &Bound<'_, PyAny>, default: StatusCode) -> StatusCode {
    result
        .getattr(intern!(result.py(), "status_code"))
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

fn validate_response_model<'py>(
    py: Python<'py>,
    model: &Bound<'py, PyAny>,
    value: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    if model.hasattr("model_validate").unwrap_or(false) {
        let kwargs = PyDict::new(py);
        kwargs.set_item("from_attributes", true)?;
        return model.call_method("model_validate", (value,), Some(&kwargs));
    }
    let typing = py.import("typing")?;
    let origin = typing.getattr("get_origin")?.call1((model,))?;
    let args = typing.getattr("get_args")?.call1((model,))?;
    let list_type = py.import("builtins")?.getattr("list")?;
    if origin.is(&list_type) {
        let item_model = args
            .try_iter()?
            .next()
            .transpose()?
            .ok_or_else(|| PyTypeError::new_err("response list model has no item type"))?;
        let input = if let Ok(input) = value.cast::<PyList>() {
            input.clone()
        } else {
            let input = PyList::empty(py);
            for item in value.try_iter()? {
                input.append(item?)?;
            }
            input
        };
        let output = PyList::empty(py);
        for item in input.iter() {
            output.append(validate_response_model(py, &item_model, &item)?)?;
        }
        return Ok(output.into_any());
    }
    let union_type = py.import("types")?.getattr("UnionType")?;
    if origin.is(&typing.getattr("Union")?) || origin.is(&union_type) {
        if value.is_none() {
            return Ok(value.clone());
        }
        for arg in args.try_iter()? {
            let arg = arg?;
            if !arg.is_none()
                && let Ok(validated) = validate_response_model(py, &arg, value)
            {
                return Ok(validated);
            }
        }
    }
    let adapter_kwargs = PyDict::new(py);
    adapter_kwargs.set_item("_parent_depth", 0)?;
    let adapter = py
        .import("pydantic")?
        .getattr("TypeAdapter")?
        .call((model,), Some(&adapter_kwargs))?;
    adapter.call_method("validate_python", (value,), Some(&PyDict::new(py)))
}

pub fn extract_base_response_data<'py>(
    result: &Bound<'py, PyAny>,
    default_status: StatusCode,
) -> BaseResponseData<'py> {
    let status_code = response_status(result, default_status);
    let content = result.getattr(intern!(result.py(), "content")).ok();
    let headers = result
        .getattr(intern!(result.py(), "headers"))
        .ok()
        .and_then(|h| h.extract::<Py<PyDict>>().ok());
    let media_type = result
        .getattr(intern!(result.py(), "media_type"))
        .ok()
        .and_then(|m| m.extract::<String>().ok());
    BaseResponseData {
        status_code,
        content,
        headers,
        media_type,
    }
}

crate::define_response_class!(PyResponse, "Response", Py<PyAny>, 200);
crate::define_response_class!(PyHTMLResponse, "HTMLResponse", String, 200);
crate::define_response_class!(PyJSONResponse, "JSONResponse", Py<PyAny>, 200);
crate::define_response_class!(PyORJSONResponse, "ORJSONResponse", Py<PyAny>, 200);
crate::define_response_class!(PyUJSONResponse, "UJSONResponse", Py<PyAny>, 200);
crate::define_response_class!(PyPlainTextResponse, "PlainTextResponse", String, 200);
crate::define_response_class!(PyStreamingResponse, "StreamingResponse", Py<PyAny>, 200);

#[pyclass(
    name = "RedirectResponse",
    module = "fastrapi.responses",
    from_py_object
)]
#[derive(Clone)]
pub struct PyRedirectResponse {
    #[pyo3(get, set)]
    pub url: String,
    #[pyo3(get, set)]
    pub status_code: u16,
    pub headers: Option<Py<PyDict>>,
    #[pyo3(get, set)]
    pub background: Option<Py<PyAny>>,
    #[pyo3(get, set)]
    pub raw_headers: Py<PyList>,
    #[pyo3(get, set)]
    pub body: Py<PyAny>,
    #[pyo3(get, set)]
    pub charset: String,
    #[pyo3(get, set)]
    pub media_type: Option<String>,
}

#[pyclass(name = "FileResponse", module = "fastrapi.responses", from_py_object)]
#[derive(Clone)]
pub struct PyFileResponse {
    #[pyo3(get, set)]
    pub path: String,
    #[pyo3(get, set)]
    pub status_code: u16,
    pub headers: Option<Py<PyDict>>,
    #[pyo3(get, set)]
    pub media_type: Option<String>,
    #[pyo3(get, set)]
    pub filename: Option<String>,
    #[pyo3(get, set)]
    pub background: Option<Py<PyAny>>,
    #[pyo3(get, set)]
    pub raw_headers: Py<PyList>,
    #[pyo3(get, set)]
    pub body: Py<PyAny>,
    #[pyo3(get, set)]
    pub charset: String,
}

crate::response_api_methods!(PyRedirectResponse, "RedirectResponse");
crate::response_api_methods!(PyFileResponse, "FileResponse");

#[pymethods]
impl PyFileResponse {
    #[new]
    #[pyo3(signature = (path, status_code=200, headers=None, media_type=None, background=None, filename=None))]
    fn new(
        py: Python<'_>,
        path: &Bound<'_, PyAny>,
        status_code: u16,
        headers: Option<Bound<'_, PyDict>>,
        media_type: Option<String>,
        background: Option<Py<PyAny>>,
        filename: Option<String>,
    ) -> PyResult<Self> {
        let path = py
            .import("os")?
            .getattr("fspath")?
            .call1((path,))?
            .extract::<String>()?;
        let media_type = Some(
            media_type
                .unwrap_or_else(|| guess_mime(filename.as_deref().unwrap_or(&path)).to_owned()),
        );
        let raw_headers = api::init_headers(
            py,
            headers.as_ref().map(Bound::as_any),
            None,
            status_code,
            media_type.as_deref(),
            "utf-8",
        )?;
        if let Some(filename) = &filename {
            let encoded = py
                .import("urllib.parse")?
                .getattr("quote")?
                .call1((filename,))?
                .extract::<String>()?;
            let disposition = if encoded == *filename {
                format!("attachment; filename=\"{filename}\"")
            } else {
                format!("attachment; filename*=utf-8''{encoded}")
            };
            let headers = api::headers(py, &raw_headers)?;
            if !headers.contains("content-disposition")? {
                headers.set_item("content-disposition", disposition)?;
            }
        }
        Ok(Self {
            path,
            status_code,
            headers: headers.map(Bound::unbind),
            media_type,
            filename,
            background,
            raw_headers,
            body: py.None(),
            charset: "utf-8".to_owned(),
        })
    }

    fn __repr__(&self) -> String {
        format!("FileResponse(path={:?})", self.path)
    }
}

#[pymethods]
impl PyRedirectResponse {
    #[new]
    #[pyo3(signature = (url, status_code=307, headers=None, background=None))]
    fn new(
        py: Python<'_>,
        url: String,
        status_code: u16,
        headers: Option<Py<PyDict>>,
        background: Option<Py<PyAny>>,
    ) -> PyResult<Self> {
        let body = PyBytes::new(py, b"").into_any().unbind();
        let raw_headers = api::init_headers(
            py,
            headers.as_ref().map(|h| h.bind(py).as_any()),
            Some(body.bind(py)),
            status_code,
            None,
            "utf-8",
        )?;
        let location = py
            .import("urllib.parse")?
            .getattr("quote")?
            .call1((&url, ":/%#?=@[]!$&'()*+,;"))?;
        api::headers(py, &raw_headers)?.set_item("location", location)?;
        Ok(Self {
            url,
            status_code,
            headers,
            background,
            raw_headers,
            body,
            charset: "utf-8".to_owned(),
            media_type: None,
        })
    }
}

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

fn apply_raw_headers(mut res: Response, result: &Bound<'_, PyAny>, cookies_only: bool) -> Response {
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
        if cookies_only && !name.eq_ignore_ascii_case(b"set-cookie") {
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
    Some(apply_raw_headers(response, result, false))
}

#[inline(always)]
pub fn convert_response_by_type(
    py: Python,
    result: &Bound<PyAny>,
    handler: &RouteHandler,
    request_headers: Option<&HeaderMap>,
) -> PyResult<Response> {
    if result.is_none() {
        return Ok(handler
            .response
            .default_status
            .unwrap_or(StatusCode::NO_CONTENT)
            .into_response());
    }

    let default_status = handler.response.default_status.unwrap_or(StatusCode::OK);
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

    if let Some(model) = &handler.response.response_model {
        validated_storage = validate_response_model(py, model.bind(py), final_result)?;
        final_result = &validated_storage;
        kind = response_kind(final_result);
    }

    let response = match handler.response.response_type {
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
            handler.response.serialization_hint,
            handler.response.dump_options.as_ref().map(|d| d.bind(py)),
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

    let request_loop = py_bridge::request_loop()
        .or_else(|| async_loop_pool().map(|pool| Python::attach(|py| Arc::new(pool.pick(py)))));
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

fn guess_mime(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "txt" | "md" => "text/plain; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "csv" => "text/csv",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        _ => "application/octet-stream",
    }
}

fn etag_for(metadata: &Metadata) -> Option<String> {
    let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(format!("\"{:x}-{:x}\"", modified.as_secs(), metadata.len()))
}

fn etag_matches(value: &str, etag: &str) -> bool {
    value
        .split(',')
        .map(str::trim)
        .any(|item| item == "*" || item.trim_start_matches("W/") == etag.trim_start_matches("W/"))
}

fn if_range_matches(headers: &HeaderMap, etag: &str, modified: Option<SystemTime>) -> bool {
    let Some(value) = headers
        .get(header::IF_RANGE)
        .and_then(|value| value.to_str().ok())
    else {
        return true;
    };
    if etag_matches(value, etag) {
        return true;
    }
    modified
        .and_then(|time| {
            httpdate::parse_http_date(value)
                .ok()
                .map(|date| time <= date)
        })
        .unwrap_or(false)
}

fn conditional_not_modified(headers: &HeaderMap, etag: &str, modified: Option<SystemTime>) -> bool {
    if let Some(value) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    {
        return etag_matches(value, etag);
    }
    modified
        .zip(
            headers
                .get(header::IF_MODIFIED_SINCE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| httpdate::parse_http_date(value).ok()),
        )
        .is_some_and(|(modified, since)| {
            modified
                .duration_since(UNIX_EPOCH)
                .is_ok_and(|value| UNIX_EPOCH + Duration::from_secs(value.as_secs()) <= since)
        })
}

fn file_stream(
    path: String,
    ranges: Vec<RangeInclusive<u64>>,
    boundary: Option<String>,
    content_type: String,
) -> Body {
    Body::from_stream(async_stream::stream! {
        let Ok(mut file) = File::open(path).await else { return; };
        let mut buffer = vec![0_u8; 64 * 1024];
        for range in ranges {
            if let Some(boundary) = &boundary {
                yield Ok::<bytes::Bytes, Infallible>(Bytes::from(format!("--{boundary}\r\nContent-Type: {content_type}\r\nContent-Range: bytes {}-{}\r\n\r\n", range.start(), range.end())));
            }
            if file.seek(SeekFrom::Start(*range.start())).await.is_err() { return; }
            let mut remaining = range.end().saturating_sub(*range.start()).saturating_add(1);
            while remaining > 0 {
                let size = remaining.min(buffer.len() as u64) as usize;
                let Ok(read) = file.read(&mut buffer[..size]).await else { return; };
                if read == 0 { return; }
                remaining -= read as u64;
                yield Ok::<bytes::Bytes, Infallible>(Bytes::copy_from_slice(&buffer[..read]));
            }
            if boundary.is_some() {
                yield Ok::<bytes::Bytes, Infallible>(Bytes::from_static(b"\r\n"));
            }
        }
        if let Some(boundary) = boundary {
            yield Ok::<bytes::Bytes, Infallible>(Bytes::from(format!("--{boundary}--\r\n")));
        }
    })
}

pub fn convert_file_response(
    py: Python<'_>,
    result: &Bound<'_, PyAny>,
    request_headers: Option<&HeaderMap>,
) -> Response {
    let get_str = |attr: &str| -> Option<String> {
        result
            .getattr(attr)
            .ok()
            .filter(|value| !value.is_none())
            .and_then(|value| value.extract().ok())
    };
    let Some(path) = get_str("path") else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let status = result
        .getattr(intern!(py, "status_code"))
        .ok()
        .and_then(|value| value.extract::<u16>().ok())
        .and_then(|value| StatusCode::from_u16(value).ok())
        .unwrap_or(StatusCode::OK);
    let Ok(metadata) = fs::metadata(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let length = metadata.len();
    let modified = metadata.modified().ok();
    let etag = etag_for(&metadata).unwrap_or_else(|| format!("\"{length:x}\""));
    let headers = request_headers.cloned().unwrap_or_default();
    if conditional_not_modified(&headers, &etag, modified) {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, &etag)
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::NOT_MODIFIED.into_response());
    }
    let use_range =
        headers.get(header::RANGE).is_some() && if_range_matches(&headers, &etag, modified);
    let ranges = if use_range {
        let value = headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let Some(ranges) = http_range_header::parse_range_header(value)
            .ok()
            .and_then(|ranges| ranges.validate(length).ok())
        else {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{length}"))
                .body(Body::empty())
                .unwrap_or_else(|_| StatusCode::RANGE_NOT_SATISFIABLE.into_response());
        };
        ranges
    } else if length == 0 {
        Vec::new()
    } else {
        once(0..=length - 1).collect()
    };
    let partial = use_range;
    let multi = ranges.len() > 1;
    let boundary = multi.then(|| format!("fastrapi-{length:x}-{status}"));
    let declared = get_str("media_type");
    let media_type = declared
        .as_deref()
        .unwrap_or_else(|| guess_mime(&path))
        .to_owned();
    let body_length = (!multi).then(|| {
        ranges.first().map_or(0, |range| {
            range.end().saturating_sub(*range.start()).saturating_add(1)
        })
    });
    let mut builder = Response::builder()
        .status(if partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            status
        })
        .header(header::ETAG, &etag)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(
            header::CONTENT_TYPE,
            boundary.as_ref().map_or_else(
                || media_type.clone(),
                |value| format!("multipart/byteranges; boundary={value}"),
            ),
        );
    if let Some(modified) = modified {
        builder = builder.header(header::LAST_MODIFIED, httpdate::fmt_http_date(modified));
    }
    if let Some(length) = body_length {
        builder = builder.header(header::CONTENT_LENGTH, length);
    }
    if partial
        && !multi
        && let Some(range) = ranges.first()
    {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {}-{}/{}", range.start(), range.end(), length),
        );
    }
    if let Some(filename) = get_str("filename") {
        let escaped = filename.replace(['"', '\\'], "");
        builder = builder.header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{escaped}\""),
        );
    }
    let response = builder
        .body(file_stream(path, ranges, boundary, media_type))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    let custom_headers = result
        .getattr(intern!(py, "headers"))
        .ok()
        .and_then(|value| value.extract::<Py<PyDict>>().ok());
    apply_raw_headers(
        apply_response_metadata(py, response, custom_headers.as_ref(), None),
        result,
        true,
    )
}
