use crate::utils::{py_json_response_with_status, py_to_response};
use axum::{
    body::Body,
    http::{HeaderName, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
};
use pyo3::{intern, prelude::*, types::PyDict, types::PyType};
use tracing::error;

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
    File,
}

static RESPONSE_KIND_CACHE: std::sync::LazyLock<
    papaya::HashMap<usize, (Py<PyType>, ResponseKind)>,
> = std::sync::LazyLock::new(|| papaya::HashMap::with_capacity(32));

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

#[pyclass(name = "FileResponse", get_all, set_all, from_py_object)]
#[derive(Clone)]
pub struct PyFileResponse {
    pub path: String,
    pub status_code: u16,
    pub headers: Option<Py<PyDict>>,
    pub media_type: Option<String>,
    pub filename: Option<String>,
}

#[pymethods]
impl PyFileResponse {
    #[new]
    #[pyo3(signature = (path, status_code=200, headers=None, media_type=None, filename=None))]
    fn new(
        path: String,
        status_code: u16,
        headers: Option<Bound<'_, PyDict>>,
        media_type: Option<String>,
        filename: Option<String>,
    ) -> Self {
        Self {
            path,
            status_code,
            headers: headers.map(|h| h.unbind()),
            media_type,
            filename,
        }
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
    crate::engine::background::spawn_response_background(py, result);

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
    } else if final_result.is_instance_of::<PyFileResponse>() {
        return Ok(convert_file_response(py, final_result));
    }

    let is_plain_value = final_result.is_instance_of::<pyo3::types::PyDict>()
        || final_result.is_instance_of::<pyo3::types::PyList>()
        || final_result.is_instance_of::<pyo3::types::PyString>()
        || final_result.is_instance_of::<pyo3::types::PyBool>()
        || final_result.is_instance_of::<pyo3::types::PyInt>()
        || final_result.is_instance_of::<pyo3::types::PyFloat>();

    let mut kind = if !is_plain_value {
        let kind = response_kind(final_result);
        if kind == ResponseKind::Base {
            return Ok(convert_auto_response(py, final_result));
        }
        kind
    } else {
        ResponseKind::Unknown
    };

    if !is_plain_value && is_starlette_response(final_result) {
        return Ok(convert_auto_response(py, final_result));
    }

    if !is_plain_value {
        match kind {
            ResponseKind::Json => return Ok(convert_json_response(py, final_result)),
            ResponseKind::Text => return Ok(convert_text_response(py, final_result)),
            ResponseKind::Html => return Ok(convert_html_response(py, final_result)),
            ResponseKind::Redirect => return Ok(convert_redirect_response(py, final_result)),
            ResponseKind::Streaming => return Ok(convert_streaming_response(py, final_result)),
            _ => {}
        }
    }

    if let Some(model) = &handler.response.response_model {
        validated_storage = model
            .bind(py)
            .call_method1("model_validate", (final_result,))?;
        final_result = &validated_storage;
        kind = response_kind(final_result);
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

        ResponseType::Json => crate::utils::py_json_response_with_dump_options(
            py,
            default_status,
            final_result,
            handler.response.serialization_hint,
            handler.response.dump_options.as_ref().map(|d| d.bind(py)),
        )?,

        ResponseType::Html => convert_html_response(py, final_result),

        ResponseType::Redirect => convert_redirect_response(py, final_result),

        ResponseType::File => convert_file_response(py, final_result),

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
            {
                convert_streaming_response(py, final_result)
            } else if final_result.is_instance_of::<PyFileResponse>() || kind == ResponseKind::File
            {
                convert_file_response(py, final_result)
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
        } else if response_kind(result) == ResponseKind::Streaming {
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
            (b.hasattr(intern!(py, "__anext__")).unwrap_or(false), b.hasattr(intern!(py, "__next__")).unwrap_or(false))
        });

        if is_async {
            loop {
                let fut = Python::attach(|py| {
                    match content.bind(py).call_method0(intern!(py, "__anext__")) {
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
                    match content.bind(py).call_method0(intern!(py, "__next__")) {
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
    apply_response_metadata(py, res, headers.as_ref(), media_type.as_deref())
}

#[inline(always)]
pub fn convert_auto_response(py: Python, result: &Bound<PyAny>) -> Response {
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
    if result.is_instance_of::<PyFileResponse>() || kind == ResponseKind::File {
        return convert_file_response(py, result);
    }

    let status = response_status(result, StatusCode::OK);
    crate::utils::py_json_response_with_status(py, status, result).unwrap_or_else(|err| {
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

pub fn convert_file_response(py: Python<'_>, result: &Bound<'_, PyAny>) -> Response {
    let get_str = |attr: &str| -> Option<String> {
        result
            .getattr(attr)
            .ok()
            .filter(|v| !v.is_none())
            .and_then(|v| v.extract::<String>().ok())
    };

    let Some(path) = get_str("path") else {
        error!("FileResponse is missing its 'path'");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let status = result
        .getattr(intern!(py, "status_code"))
        .ok()
        .and_then(|s| s.extract::<u16>().ok())
        .and_then(|s| StatusCode::from_u16(s).ok())
        .unwrap_or(StatusCode::OK);

    match std::fs::read(&path) {
        Ok(bytes) => {
            let declared = get_str("media_type");
            let media_type = declared.as_deref().unwrap_or_else(|| guess_mime(&path));
            let mut builder = Response::builder()
                .status(status)
                .header(
                    header::CONTENT_TYPE,
                    HeaderValue::from_str(media_type)
                        .unwrap_or(HeaderValue::from_static("application/octet-stream")),
                )
                .header(header::CONTENT_LENGTH, bytes.len());

            if let Some(filename) = get_str("filename") {
                let escaped = filename.replace(['"', '\\'], "");
                builder = builder.header(
                    header::CONTENT_DISPOSITION,
                    HeaderValue::from_str(&format!("attachment; filename=\"{escaped}\""))
                        .unwrap_or(HeaderValue::from_static("attachment")),
                );
            }

            let res = builder
                .body(Body::from(bytes))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            let headers = result
                .getattr(intern!(py, "headers"))
                .ok()
                .and_then(|h| h.extract::<Py<PyDict>>().ok());
            apply_response_metadata(py, res, headers.as_ref(), None)
        }
        Err(err) => {
            let status = match err.kind() {
                std::io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
                std::io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            error!("FileResponse failed to read '{path}': {err}");
            status.into_response()
        }
    }
}
