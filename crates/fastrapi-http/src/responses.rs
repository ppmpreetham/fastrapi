//! This module contains the Python response classes, for example `JSONResponse`, `RedirectResponse`, and `FileResponse`.
//! It also finds the kind of a response object and validates the response model.

use super::response_file::guess_mime;
use axum::http::StatusCode;
use papaya::HashMap;
use pyo3::exceptions::PyTypeError;
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

pub(crate) fn is_starlette_response(result: &Bound<'_, PyAny>) -> bool {
    let py = result.py();
    result.hasattr(intern!(py, "body")).unwrap_or(false)
        && result.hasattr(intern!(py, "status_code")).unwrap_or(false)
        && result.hasattr(intern!(py, "headers")).unwrap_or(false)
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

pub(crate) fn validate_response_model<'py>(
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

crate::macros::define_response_class!(PyResponse, "Response", Py<PyAny>, 200);
crate::macros::define_response_class!(PyHTMLResponse, "HTMLResponse", String, 200);
crate::macros::define_response_class!(PyJSONResponse, "JSONResponse", Py<PyAny>, 200);
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
    PyORJSONResponse, "ORJSONResponse", Py<PyAny>, 200);
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
    PyUJSONResponse, "UJSONResponse", Py<PyAny>, 200);
crate::macros::define_response_class!(PyPlainTextResponse, "PlainTextResponse", String, 200);
crate::macros::define_response_class!(PyStreamingResponse, "StreamingResponse", Py<PyAny>, 200);

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

crate::macros::response_api_methods!(PyRedirectResponse, "RedirectResponse");
crate::macros::response_api_methods!(PyFileResponse, "FileResponse");

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
                .getattr("quote")?
                .call1((filename,))?
                .extract::<String>()?;
            let disposition = if encoded == *filename {
                format!("attachment; filename=\"{filename}\"")
            } else {
                format!("attachment; filename*=utf-8''{encoded}")
            };
            let headers = crate::response_api::headers(py, &raw_headers)?;
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
            .getattr("quote")?
            .call1((&url, ":/%#?=@[]!$&'()*+,;"))?;
        crate::response_api::headers(py, &raw_headers)?.set_item("location", location)?;
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
