//! This module contains the classes for Server-Sent Events (SSE).
//! `EventSourceResponse` marks a route that streams events.
//! `ServerSentEvent` is one event that the server encodes in the `text/event-stream` format.

use crate::response_api::init_headers;
use bytes::Bytes;
use fastrapi_pyutil::json::py_any_to_json;
use pyo3::Bound;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyTuple, PyType};

/// Streaming response with `text/event-stream` media type.
///
/// Use as `response_class=EventSourceResponse` on a *path operation* that uses `yield`
/// to enable Server Sent Events (SSE) responses.
///
/// Works with **any HTTP method** (`GET`, `POST`, etc.), which makes it compatible
/// with protocols like MCP that stream SSE over `POST`.
///
/// The actual encoding logic lives in the FastAPI routing layer. This class
/// serves mainly as a marker and sets the correct `Content-Type`.
#[pyclass(
    name = "EventSourceResponse",
    module = "fastrapi.responses",
    get_all,
    set_all,
    from_py_object
)]
#[derive(Clone)]
pub struct PyEventSourceResponse {
    pub content: Py<PyAny>,
    pub status_code: u16,
    pub headers: Option<Py<PyDict>>,
    pub media_type: Option<String>,
    pub background: Option<Py<PyAny>>,
    pub ping: Option<f64>,
    pub sep: String,
    pub raw_headers: Py<PyList>,
    pub body: Py<PyAny>,
    pub charset: String,
}

#[pymethods]
impl PyEventSourceResponse {
    #[new]
    #[pyo3(signature = (content, status_code=200, headers=None, media_type=None, background=None, ping=15.0, sep="\r\n".to_string()))]
    fn new(
        py: Python<'_>,
        content: Py<PyAny>,
        status_code: u16,
        headers: Option<Py<PyDict>>,
        media_type: Option<String>,
        background: Option<Py<PyAny>>,
        ping: Option<f64>,
        sep: String,
    ) -> PyResult<Self> {
        if ping.is_some_and(|value| value < 0.0) {
            return Err(PyValueError::new_err("ping must be non-negative"));
        }
        if sep.is_empty() {
            return Err(PyValueError::new_err("sep must not be empty"));
        }
        let raw_headers = init_headers(
            py,
            headers.as_ref().map(|h| h.bind(py).as_any()),
            None,
            status_code,
            Some("text/event-stream"),
            "utf-8",
        )?;
        Ok(Self {
            content,
            status_code,
            headers,
            media_type: Some(media_type.unwrap_or_else(|| "text/event-stream".to_owned())),
            background,
            ping,
            sep,
            raw_headers,
            body: py.None(),
            charset: "utf-8".to_owned(),
        })
    }
}

crate::macros::response_api_methods!(PyEventSourceResponse, "EventSourceResponse");

/// Represents a single Server-Sent Event.
///
/// When `yield`ed from a *path operation function* that uses
/// `response_class=EventSourceResponse`, each `ServerSentEvent` is encoded
/// into the [SSE wire format](https://html.spec.whatwg.org/multipage/server-sent-events.html#parsing-an-event-stream)
/// (`text/event-stream`).
///
/// If you yield a plain object (dict, Pydantic model, etc.) instead, it is
/// automatically JSON-encoded and sent as the `data:` field.
///
/// All `data` values **including plain strings** are JSON-serialized.
///
/// For example, `data="hello"` produces `data: "hello"` on the wire (with
/// quotes).
#[pyclass(
    name = "ServerSentEvent",
    module = "fastrapi.sse",
    get_all,
    skip_from_py_object
)]
#[derive(Clone)]
pub struct PyServerSentEvent {
    pub data: Py<PyAny>,
    pub raw_data: Option<String>,
    pub event: Option<String>,
    pub id: Option<String>,
    pub retry: Option<i64>,
    pub comment: Option<String>,
}

pub const KEEPALIVE_COMMENT: &[u8] = b": ping\n\n";

#[pymethods]
impl PyServerSentEvent {
    #[new]
    #[pyo3(signature = (*, data=None, raw_data=None, event=None, id=None, retry=None, comment=None))]
    fn new(
        py: Python<'_>,
        data: Option<Py<PyAny>>,
        raw_data: Option<String>,
        event: Option<String>,
        id: Option<String>,
        retry: Option<i64>,
        comment: Option<String>,
    ) -> PyResult<Self> {
        if data.is_some() && raw_data.is_some() {
            return Err(PyValueError::new_err(
                "data and raw_data are mutually exclusive",
            ));
        }
        if event.as_deref().is_some_and(single_line_invalid)
            || id.as_deref().is_some_and(single_line_invalid)
            || id.as_deref().is_some_and(|value| value.contains('\0'))
        {
            return Err(PyValueError::new_err(
                "event and id must be valid single-line values",
            ));
        }
        if retry.is_some_and(|value| value < 0) {
            return Err(PyValueError::new_err("retry must be non-negative"));
        }
        Ok(Self {
            data: data.unwrap_or_else(|| py.None()),
            raw_data,
            event,
            id,
            retry,
            comment,
        })
    }

    #[pyo3(signature = (*_args, **kwargs))]
    fn model_dump(
        &self,
        py: Python<'_>,
        _args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyDict>> {
        let result = PyDict::new(py);
        let exclude_none = kwargs
            .and_then(|value| value.get_item("exclude_none").ok().flatten())
            .and_then(|value| value.extract::<bool>().ok())
            .unwrap_or(false);
        if !self.data.bind(py).is_none() || !exclude_none {
            result.set_item("data", self.data.bind(py))?;
        }
        if !exclude_none || self.raw_data.is_some() {
            result.set_item("raw_data", self.raw_data.as_deref())?;
        }
        if !exclude_none || self.event.is_some() {
            result.set_item("event", self.event.as_deref())?;
        }
        if !exclude_none || self.id.is_some() {
            result.set_item("id", self.id.as_deref())?;
        }
        if !exclude_none || self.retry.is_some() {
            result.set_item("retry", self.retry)?;
        }
        if !exclude_none || self.comment.is_some() {
            result.set_item("comment", self.comment.as_deref())?;
        }
        if let Some(kwargs) = kwargs {
            if let Some(include) = kwargs.get_item("include")?
                && let Ok(include) = include.extract::<Vec<String>>()
            {
                for key in ["data", "raw_data", "event", "id", "retry", "comment"] {
                    if !include.iter().any(|item| item == key) {
                        result.del_item(key)?;
                    }
                }
            }
            if let Some(exclude) = kwargs.get_item("exclude")?
                && let Ok(exclude) = exclude.extract::<Vec<String>>()
            {
                for key in exclude {
                    result.del_item(key)?;
                }
            }
        }
        Ok(result.unbind())
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn model_dump_json(
        &self,
        py: Python<'_>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<String> {
        let dumped = self.model_dump(py, args, kwargs)?;
        py.import("json")?
            .getattr("dumps")?
            .call1((dumped.bind(py),))?
            .extract()
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn dict(
        &self,
        py: Python<'_>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyDict>> {
        self.model_dump(py, args, kwargs)
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn json(
        &self,
        py: Python<'_>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<String> {
        self.model_dump_json(py, args, kwargs)
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn copy(
        &self,
        py: Python<'_>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        self.model_copy(py, args, kwargs)
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn model_copy(
        &self,
        py: Python<'_>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let deep = kwargs
            .and_then(|value| value.get_item("deep").ok().flatten())
            .and_then(|value| value.extract::<bool>().ok())
            .unwrap_or(false);
        let data = self.model_dump(py, args, None)?;
        if let Some(kwargs) = kwargs
            && let Some(update) = kwargs.get_item("update")?
            && !update.is_none()
        {
            data.bind(py).update(update.cast()?)?;
        }
        if deep {
            let copied = py
                .import("copy")?
                .call_method1("deepcopy", (data.bind(py),))?;
            return py
                .get_type::<Self>()
                .call((), Some(copied.cast::<PyDict>()?))
                .map(Bound::unbind);
        }
        py.get_type::<Self>()
            .call((), Some(data.bind(py)))
            .map(Bound::unbind)
    }

    #[pyo3(signature = (_context))]
    const fn model_post_init(&self, _context: &Bound<'_, PyAny>) {}

    #[classmethod]
    fn model_validate(cls: &Bound<'_, PyType>, value: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        if let Ok(event) = value.cast::<Self>() {
            return Ok(event.clone().unbind().into_any());
        }
        let dict = value.cast::<PyDict>()?;
        let kwargs = dict.copy()?.into_any().cast_into::<PyDict>()?;
        cls.call((), Some(&kwargs)).map(Bound::unbind)
    }
}

#[pyfunction]
#[pyo3(name = "format_sse_event")]
#[pyo3(signature = (*, data_str=None, event=None, id=None, retry=None, comment=None))]
#[must_use]
pub fn format_sse_event_py(
    data_str: Option<String>,
    event: Option<String>,
    id: Option<String>,
    retry: Option<i64>,
    comment: Option<String>,
) -> Vec<u8> {
    let mut output = String::new();
    for (prefix, value) in [
        (": ", comment),
        ("event: ", event),
        ("data: ", data_str),
        ("id: ", id),
    ] {
        if let Some(value) = value {
            for line in value.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
                output.push_str(prefix);
                output.push_str(line);
                output.push('\n');
            }
        }
    }
    if let Some(retry) = retry {
        output.push_str("retry: ");
        output.push_str(&retry.to_string());
        output.push('\n');
    }
    output.push('\n');
    output.into_bytes()
}

fn single_line_invalid(value: &str) -> bool {
    value.contains('\r') || value.contains('\n')
}

pub fn format_sse_event(
    py: Python<'_>,
    data: Option<&Bound<'_, PyAny>>,
    raw_data: Option<&str>,
    event: Option<&str>,
    id: Option<&str>,
    retry: Option<i64>,
    comment: Option<&str>,
    sep: &str,
) -> bytes::Bytes {
    let data = raw_data.map(str::to_owned).or_else(|| {
        data.map(|value| simd_json::to_string(&py_any_to_json(py, value)).unwrap_or_default())
    });
    let mut output = String::new();
    if let Some(comment) = comment {
        for line in comment
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
        {
            output.push(':');
            output.push(' ');
            output.push_str(line);
            output.push_str(sep);
        }
    }
    if let Some(event) = event {
        output.push_str("event: ");
        output.push_str(event);
        output.push_str(sep);
    }
    if let Some(data) = data {
        for line in data
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
        {
            output.push_str("data: ");
            output.push_str(line);
            output.push_str(sep);
        }
    }
    if let Some(id) = id {
        output.push_str("id: ");
        output.push_str(id);
        output.push_str(sep);
    }
    if let Some(retry) = retry {
        output.push_str("retry: ");
        output.push_str(&retry.to_string());
        output.push_str(sep);
    }
    output.push_str(sep);
    Bytes::from(output)
}

#[must_use]
pub fn event_fields<'py>(
    py: Python<'py>,
    value: &Bound<'py, PyAny>,
) -> Option<(
    Option<Bound<'py, PyAny>>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
)> {
    let event = value.cast::<PyServerSentEvent>().ok()?;
    let event = event.borrow();
    Some((
        (!event.data.bind(py).is_none()).then(|| event.data.bind(py).clone()),
        event.raw_data.clone(),
        event.event.clone(),
        event.id.clone(),
        event.retry,
        event.comment.clone(),
    ))
}

pub fn add_sse_module(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let module = PyModule::new(parent.py(), "sse")?;
    module.add_class::<PyServerSentEvent>()?;
    module.add_function(wrap_pyfunction!(format_sse_event_py, &module)?)?;
    module.add("KEEPALIVE_COMMENT", KEEPALIVE_COMMENT)?;
    parent.add_submodule(&module)
}

pub fn add_sse_response_module(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let module = PyModule::new(parent.py(), "responses")?;
    module.add_class::<PyEventSourceResponse>()?;
    parent.add_submodule(&module)
}

pub fn register_sse_api(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(format_sse_event_py, module)?)?;
    module.add("KEEPALIVE_COMMENT", KEEPALIVE_COMMENT)?;
    Ok(())
}

#[must_use]
pub fn sse_options(result: &Bound<'_, PyAny>) -> (Option<f64>, String) {
    let ping = result
        .getattr("ping")
        .ok()
        .and_then(|value| value.extract::<Option<f64>>().ok())
        .flatten();
    let sep = result
        .getattr("sep")
        .ok()
        .and_then(|value| value.extract::<String>().ok())
        .unwrap_or_else(|| "\r\n".to_owned());
    (ping, sep)
}

#[must_use]
pub fn sse_dict_fields<'py>(
    value: &Bound<'py, PyAny>,
) -> Option<(
    Option<Bound<'py, PyAny>>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
)> {
    let item = value.cast::<PyDict>().ok()?;
    let data = item.get_item("data").ok().flatten();
    let raw_data = item
        .get_item("raw_data")
        .ok()
        .flatten()
        .and_then(|value| value.extract().ok());
    let event = item
        .get_item("event")
        .ok()
        .flatten()
        .and_then(|value| value.extract().ok());
    let id = item
        .get_item("id")
        .ok()
        .flatten()
        .and_then(|value| value.extract().ok());
    let retry = item
        .get_item("retry")
        .ok()
        .flatten()
        .and_then(|value| value.extract().ok());
    let comment = item
        .get_item("comment")
        .ok()
        .flatten()
        .and_then(|value| value.extract().ok());
    Some((data, raw_data, event, id, retry, comment))
}
