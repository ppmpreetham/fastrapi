//! This module contains the Python response classes, for example `JSONResponse`, `RedirectResponse`, and `FileResponse`.
//! It also finds the kind of a response object and validates the response model.

use super::response_file::guess_mime;
use axum::http::StatusCode;
use papaya::HashMap;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyType};
use pyo3::{Bound, Py, PyAny, intern, pyclass, pymethods};
use std::sync::LazyLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResponseKind {
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
pub(crate) fn response_kind(result: &Bound<'_, PyAny>) -> ResponseKind {
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
    let own = kind_from_name(ty);
    if own != ResponseKind::Unknown {
        return own;
    }
    ty.mro()
        .iter()
        .skip(1)
        .filter_map(|base| base.cast_into::<PyType>().ok())
        .filter(|base| {
            base.module().is_ok_and(|module| {
                module.to_str().is_ok_and(|module| {
                    ["starlette", "fastapi", "fastrapi"].iter().any(|root| {
                        module
                            .strip_prefix(root)
                            .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
                    })
                })
            })
        })
        .map(|base| kind_from_name(&base))
        .find(|kind| *kind != ResponseKind::Unknown)
        .unwrap_or(ResponseKind::Unknown)
}

fn kind_from_name(ty: &Bound<'_, PyType>) -> ResponseKind {
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

pub(crate) fn response_status(result: &Bound<'_, PyAny>, default: StatusCode) -> StatusCode {
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

crate::macros::define_response_class!(@base PyResponse, "Response", 200);

impl PyResponse {
    pub fn empty(py: Python<'_>) -> PyResult<Self> {
        Ok(Self {
            content: py.None(),
            status_code: 200,
            headers: None,
            media_type: None,
            background: None,
            raw_headers: PyList::empty(py).unbind(),
            body: PyBytes::new(py, b"").into_any().unbind(),
            charset: "utf-8".to_owned(),
        })
    }
}
crate::macros::define_response_class!(PyHTMLResponse, "HTMLResponse", 200);
crate::macros::define_response_class!(PyJSONResponse, "JSONResponse", 200);
crate::macros::define_response_class!(
    /// JSON response using the orjson library to serialize data to JSON.
    ///
    /// **Deprecated**: `ORJSONResponse` is deprecated. FastAPI now serializes data
    /// directly to JSON bytes via Pydantic when a return type or response model is
    /// set, which is faster and doesn't need a custom response class.
    ///
    /// Read more in the
    /// [FastAPI docs for Custom Response](https://fastapi.tiangolo.com/advanced/custom-response/#orjson-or-response-model)
    /// and the
    /// [FastAPI docs for Response Model](https://fastapi.tiangolo.com/tutorial/response-model/).
    ///
    /// **Note**: `orjson` is not included with FastAPI and must be installed
    /// separately, e.g. `pip install orjson`.
    PyORJSONResponse, "ORJSONResponse", 200);
crate::macros::define_response_class!(
    /// JSON response using the ujson library to serialize data to JSON.
    ///
    /// **Deprecated**: `UJSONResponse` is deprecated. FastAPI now serializes data
    /// directly to JSON bytes via Pydantic when a return type or response model is
    /// set, which is faster and doesn't need a custom response class.
    ///
    /// Read more in the
    /// [FastAPI docs for Custom Response](https://fastapi.tiangolo.com/advanced/custom-response/#orjson-or-response-model)
    /// and the
    /// [FastAPI docs for Response Model](https://fastapi.tiangolo.com/tutorial/response-model/).
    ///
    /// **Note**: `ujson` is not included with FastAPI and must be installed
    /// separately, e.g. `pip install ujson`.
    PyUJSONResponse, "UJSONResponse", 200);
crate::macros::define_response_class!(PyPlainTextResponse, "PlainTextResponse", 200);
crate::macros::define_response_class!(PyStreamingResponse, "StreamingResponse", 200);

#[pyclass(
    name = "RedirectResponse",
    module = "fastrapi.responses",
    extends = PyResponse,
    subclass,
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

#[pyclass(
    name = "FileResponse",
    module = "fastrapi.responses",
    extends = PyResponse,
    subclass,
    from_py_object
)]
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
    #[pyo3(get, set)]
    pub stat_result: Option<Py<PyAny>>,
    #[pyo3(get, set)]
    pub content_disposition_type: String,
}

crate::macros::response_api_methods!(PyRedirectResponse, "RedirectResponse");
crate::macros::response_api_methods!(PyFileResponse, "FileResponse");

#[pymethods]
impl PyFileResponse {
    #[new]
    #[pyo3(signature = (
        path,
        status_code=200,
        headers=None,
        media_type=None,
        background=None,
        filename=None,
        stat_result=None,
        method=None,
        content_disposition_type="attachment".to_owned()
    ))]
    #[expect(
        clippy::too_many_arguments,
        reason = "the signature copies Starlette's FileResponse"
    )]
    fn new(
        py: Python<'_>,
        path: &Bound<'_, PyAny>,
        status_code: u16,
        headers: Option<Bound<'_, PyDict>>,
        media_type: Option<String>,
        background: Option<Py<PyAny>>,
        filename: Option<String>,
        stat_result: Option<Py<PyAny>>,
        #[expect(
            unused_variables,
            reason = "part of Starlette's FileResponse signature"
        )]
        method: Option<&str>,
        content_disposition_type: String,
    ) -> PyResult<PyClassInitializer<Self>> {
        let path = py
            .import("os")?
            .getattr(pyo3::intern!(py, "fspath"))?
            .call1((path,))?
            .extract::<String>()?;
        let media_type = Some(
            media_type
                .unwrap_or_else(|| guess_mime(filename.as_deref().unwrap_or(&path)).to_owned()),
        );
        let raw_headers = crate::response_api::init_headers(
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
                .getattr(pyo3::intern!(py, "quote"))?
                .call1((filename,))?
                .extract::<String>()?;
            let disposition = if encoded == *filename {
                format!("{content_disposition_type}; filename=\"{filename}\"")
            } else {
                format!("{content_disposition_type}; filename*=utf-8''{encoded}")
            };
            let headers = crate::response_api::headers(py, &raw_headers)?;
            if !headers.contains("content-disposition")? {
                headers.set_item("content-disposition", disposition)?;
            }
        }
        Ok(
            PyClassInitializer::from(PyResponse::empty(py)?).add_subclass(Self {
                path,
                status_code,
                headers: headers.map(Bound::unbind),
                media_type,
                filename,
                background,
                raw_headers,
                body: py.None(),
                charset: "utf-8".to_owned(),
                stat_result,
                content_disposition_type,
            }),
        )
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
        url: &Bound<'_, PyAny>,
        status_code: u16,
        headers: Option<Py<PyDict>>,
        background: Option<Py<PyAny>>,
    ) -> PyResult<PyClassInitializer<Self>> {
        let url: String = url.str()?.extract()?;
        let body = PyBytes::new(py, b"").into_any().unbind();
        let raw_headers = crate::response_api::init_headers(
            py,
            headers.as_ref().map(|h| h.bind(py).as_any()),
            Some(body.bind(py)),
            status_code,
            None,
            "utf-8",
        )?;
        let location = py
            .import("urllib.parse")?
            .getattr(pyo3::intern!(py, "quote"))?
            .call1((&url, ":/%#?=@[]!$&'()*+,;"))?;
        crate::response_api::headers(py, &raw_headers)?
            .set_item(pyo3::intern!(py, "location"), location)?;
        Ok(
            PyClassInitializer::from(PyResponse::empty(py)?).add_subclass(Self {
                url,
                status_code,
                headers,
                background,
                raw_headers,
                body,
                charset: "utf-8".to_owned(),
                media_type: None,
            }),
        )
    }
}
