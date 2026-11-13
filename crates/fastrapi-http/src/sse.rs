//! This module contains the classes for Server-Sent Events (SSE).
//! `EventSourceResponse` marks a route that streams events.
//! `ServerSentEvent` is one event that the server encodes in the `text/event-stream` format.

use crate::response_api::init_headers;
use crate::responses::{PyResponse, PyStreamingResponse};
use bytes::Bytes;
use std::fmt::Write as _;
use fastrapi_pyutil::cached_py_import;
use pyo3::Bound;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{
    IntoPyDict, PyAny, PyBool, PyDict, PyFloat, PyInt, PyList, PySet, PyString, PyTuple, PyType,
};

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
    extends = PyStreamingResponse,
    subclass,
    get_all,
    set_all
)]
pub struct PyEventSourceResponse {
    pub ping: Option<f64>,
    pub sep: String,
}

#[pymethods]
impl PyEventSourceResponse {
    #[new]
    #[pyo3(signature = (content, status_code=200, headers=None, media_type=None, background=None, ping=15.0, sep="
".to_string()))]
    fn new(
        py: Python<'_>,
        content: Py<PyAny>,
        status_code: u16,
        headers: Option<crate::response_api::HeadersArg>,
        media_type: Option<String>,
        background: Option<Py<PyAny>>,
        ping: Option<f64>,
        sep: String,
    ) -> PyResult<PyClassInitializer<Self>> {
        let headers = headers.map(|headers| headers.0);
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
        let stream = PyStreamingResponse {
            content,
            status_code,
            headers,
            media_type: Some(media_type.unwrap_or_else(|| "text/event-stream".to_owned())),
            background,
            raw_headers,
            body: py.None(),
            charset: "utf-8".to_owned(),
        };
        Ok(PyClassInitializer::from(PyResponse::empty(py)?)
            .add_subclass(stream)
            .add_subclass(Self { ping, sep }))
    }
}

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
#[pyclass(name = "ServerSentEvent", module = "fastrapi.sse", skip_from_py_object)]
#[derive(Clone)]
pub struct PyServerSentEvent {
    #[pyo3(get)]
    pub data: Py<PyAny>,
    #[pyo3(get)]
    pub raw_data: Option<String>,
    #[pyo3(get)]
    pub event: Option<String>,
    #[pyo3(get)]
    pub id: Option<String>,
    #[pyo3(get)]
    pub retry: Option<i64>,
    #[pyo3(get)]
    pub comment: Option<String>,
    fields_set: u8,
}

const EVENT_FIELDS: [&str; 6] = ["data", "raw_data", "event", "id", "retry", "comment"];

static EVENT_MODEL: PyOnceLock<Py<PyAny>> = PyOnceLock::new();

fn event_model(py: Python<'_>) -> PyResult<&Bound<'_, PyAny>> {
    EVENT_MODEL
        .get_or_try_init(py, || {
            let typing = py.import("typing")?;
            let optional = typing.getattr("Optional")?;
            let text = optional.get_item(py.get_type::<PyString>())?;
            let retry = typing.getattr("Annotated")?.get_item((
                optional.get_item(py.get_type::<PyInt>())?,
                py.import("pydantic")?
                    .getattr("Field")?
                    .call((), Some(&[("ge", 0)].into_py_dict(py)?))?,
            ))?;
            let fields = PyDict::new(py);
            fields.set_item(
                "__doc__",
                py.get_type::<PyServerSentEvent>().getattr("__doc__")?,
            )?;
            fields.set_item("data", (typing.getattr("Any")?, py.None()))?;
            for name in ["raw_data", "event", "id"] {
                fields.set_item(name, (&text, py.None()))?;
            }
            fields.set_item("retry", (retry, py.None()))?;
            fields.set_item("comment", (&text, py.None()))?;
            py.import("pydantic")?
                .getattr("create_model")?
                .call(("ServerSentEvent",), Some(&fields))
                .map(Bound::unbind)
        })
        .map(|model| model.bind(py))
}

pub const KEEPALIVE_COMMENT: &[u8] = b": ping\n\n";

#[pymethods]
impl PyServerSentEvent {
    #[new]
    #[pyo3(signature = (*, data=None, raw_data=None, event=None, id=None, retry=None, comment=None, **_given))]
    #[expect(
        clippy::too_many_arguments,
        reason = "the signature copies the FastAPI ServerSentEvent model"
    )]
    fn new(
        py: Python<'_>,
        data: Option<Py<PyAny>>,
        raw_data: Option<String>,
        event: Option<String>,
        id: Option<String>,
        retry: Option<i64>,
        comment: Option<String>,
        _given: Option<&Bound<'_, PyDict>>,
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
        let fields_set = [
            data.is_some(),
            raw_data.is_some(),
            event.is_some(),
            id.is_some(),
            retry.is_some(),
            comment.is_some(),
        ]
        .iter()
        .enumerate()
        .fold(0, |mask, (bit, set)| mask | (u8::from(*set) << bit));
        Ok(Self {
            data: data.unwrap_or_else(|| py.None()),
            raw_data,
            event,
            id,
            retry,
            comment,
            fields_set,
        })
    }

    #[classattr]
    fn model_fields(py: Python<'_>) -> PyResult<Py<PyAny>> {
        event_model(py)?.getattr("model_fields").map(Bound::unbind)
    }

    #[getter]
    fn model_fields_set<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PySet>> {
        PySet::new(
            py,
            EVENT_FIELDS
                .iter()
                .enumerate()
                .filter(|(bit, _)| self.fields_set & (1 << bit) != 0)
                .map(|(_, name)| *name),
        )
    }

    #[classmethod]
    #[pyo3(signature = (**kwargs))]
    fn model_json_schema<'py>(
        cls: &Bound<'py, PyType>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        event_model(cls.py())?.call_method("model_json_schema", (), kwargs)
    }

    #[classmethod]
    #[pyo3(signature = (value, **_options))]
    fn model_validate<'py>(
        cls: &Bound<'py, PyType>,
        value: &Bound<'py, PyAny>,
        _options: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        if value.is_instance(cls)? {
            return Ok(value.clone());
        }
        cls.call((), Some(value.cast::<PyDict>()?))
    }

    #[pyo3(signature = (**kwargs))]
    fn model_dump<'py>(
        &self,
        py: Python<'py>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.as_model(py)?.call_method("model_dump", (), kwargs)
    }

    #[pyo3(signature = (**kwargs))]
    fn model_dump_json<'py>(
        &self,
        py: Python<'py>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.as_model(py)?
            .call_method("model_dump_json", (), kwargs)
    }

    #[pyo3(signature = (*, update=None, deep=false))]
    fn model_copy<'py>(
        &self,
        py: Python<'py>,
        update: Option<&Bound<'py, PyDict>>,
        deep: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let fields = self.fields(py)?;
        if let Some(update) = update {
            fields.update(update.as_mapping())?;
        }
        let fields = if deep {
            py.import("copy")?
                .call_method1("deepcopy", (fields,))?
                .cast_into::<PyDict>()?
        } else {
            fields
        };
        py.get_type::<Self>().call((), Some(&fields))
    }

    #[pyo3(signature = (*, update=None, deep=false))]
    fn copy<'py>(
        &self,
        py: Python<'py>,
        update: Option<&Bound<'py, PyDict>>,
        deep: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.model_copy(py, update, deep)
    }

    #[pyo3(signature = (**kwargs))]
    fn dict<'py>(
        &self,
        py: Python<'py>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.as_model(py)?.call_method("dict", (), kwargs)
    }

    #[pyo3(signature = (**kwargs))]
    fn json<'py>(
        &self,
        py: Python<'py>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.as_model(py)?.call_method("json", (), kwargs)
    }

    #[pyo3(signature = (_context))]
    const fn model_post_init(&self, _context: &Bound<'_, PyAny>) {}

    fn __eq__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<bool> {
        match other.cast::<Self>() {
            Ok(other) => self.fields(py)?.eq(other.borrow().fields(py)?),
            Err(_) => Ok(false),
        }
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(self.as_model(py)?.repr()?.to_string())
    }

    fn __str__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(self.as_model(py)?.str()?.to_string())
    }
}

impl PyServerSentEvent {
    fn fields<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let fields = PyDict::new(py);
        fields.set_item("data", self.data.bind(py))?;
        fields.set_item("raw_data", self.raw_data.as_deref())?;
        fields.set_item("event", self.event.as_deref())?;
        fields.set_item("id", self.id.as_deref())?;
        fields.set_item("retry", self.retry)?;
        fields.set_item("comment", self.comment.as_deref())?;
        Ok(fields)
    }

    fn as_model<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let fields = self.fields(py)?;
        fields.set_item("_fields_set", self.model_fields_set(py)?)?;
        event_model(py)?.call_method("model_construct", (), Some(&fields))
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

cached_py_import!(JSONABLE_ENCODER, "fastrapi.encoders", "jsonable_encoder");

fn push_json_float(out: &mut String, value: f64) {
    if !value.is_finite() {
        out.push_str("null");
        return;
    }
    if value == 0.0 {
        out.push_str(if value.is_sign_negative() { "-0.0" } else { "0.0" });
        return;
    }
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or_default();
    let mantissa = mantissa.strip_prefix('-').map_or(mantissa, |mantissa| {
        out.push('-');
        mantissa
    });
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    if (-4..16).contains(&exponent) {
        if let Ok(whole) = usize::try_from(exponent + 1) {
            if digits.len() <= whole {
                out.push_str(&digits);
                out.extend(std::iter::repeat_n('0', whole - digits.len()));
                out.push_str(".0");
            } else {
                out.push_str(&digits[..whole]);
                out.push('.');
                out.push_str(&digits[whole..]);
            }
        } else {
            out.push_str("0.");
            out.extend(std::iter::repeat_n('0', exponent.unsigned_abs() as usize - 1));
            out.push_str(&digits);
        }
        return;
    }
    out.push_str(&digits[..1]);
    if digits.len() > 1 {
        out.push('.');
        out.push_str(&digits[1..]);
    }
    out.push('e');
    out.push(if exponent < 0 { '-' } else { '+' });
    if exponent.unsigned_abs() < 10 {
        out.push('0');
    }
    _ = write!(out, "{}", exponent.unsigned_abs());
}

fn push_json_string(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(ch),
            _ => {
                let mut units = [0u16; 2];
                for unit in ch.encode_utf16(&mut units) {
                    _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

fn push_json_key(out: &mut String, key: &Bound<'_, PyAny>) -> PyResult<()> {
    if let Ok(text) = key.cast::<PyString>() {
        push_json_string(out, &text.to_cow()?);
    } else if let Ok(flag) = key.cast::<PyBool>() {
        out.push_str(if flag.is_true() { "\"true\"" } else { "\"false\"" });
    } else if key.is_none() {
        out.push_str("\"null\"");
    } else if let Ok(number) = key.cast::<PyFloat>() {
        let mut text = String::new();
        push_json_float(&mut text, number.value());
        push_json_string(out, &text);
    } else {
        push_json_string(out, &key.str()?.to_cow()?);
    }
    Ok(())
}

fn push_json_dumps(out: &mut String, value: &Bound<'_, PyAny>, encoded: bool) -> PyResult<()> {
    if value.is_none() {
        out.push_str("null");
    } else if let Ok(flag) = value.cast_exact::<PyBool>() {
        out.push_str(if flag.is_true() { "true" } else { "false" });
    } else if let Some(text) = value
        .cast_exact::<PyString>()
        .ok()
        .or_else(|| encoded.then(|| value.cast::<PyString>().ok()).flatten())
    {
        push_json_string(out, &text.to_cow()?);
    } else if let Some(number) = value
        .cast_exact::<PyInt>()
        .ok()
        .or_else(|| encoded.then(|| value.cast::<PyInt>().ok()).flatten())
    {
        match number.extract::<i64>() {
            Ok(number) => _ = write!(out, "{number}"),
            Err(_) => out.push_str(&number.str()?.to_cow()?),
        }
    } else if let Some(number) = value
        .cast_exact::<PyFloat>()
        .ok()
        .or_else(|| encoded.then(|| value.cast::<PyFloat>().ok()).flatten())
    {
        push_json_float(out, number.value());
    } else if let Ok(dict) = value.cast_exact::<PyDict>() {
        out.push('{');
        for (index, (key, item)) in dict.iter().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            push_json_key(out, &key)?;
            out.push_str(": ");
            push_json_dumps(out, &item, encoded)?;
        }
        out.push('}');
    } else if value.is_exact_instance_of::<PyList>() || value.is_exact_instance_of::<PyTuple>() {
        out.push('[');
        for (index, item) in value.try_iter()?.enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            push_json_dumps(out, &item?, encoded)?;
        }
        out.push(']');
    } else if encoded {
        return Err(PyValueError::new_err("value is not JSON serializable"));
    } else {
        let encoded = JSONABLE_ENCODER.get(value.py())?.call1((value,))?;
        push_json_dumps(out, &encoded, true)?;
    }
    Ok(())
}

pub fn json_dumps(value: &Bound<'_, PyAny>) -> PyResult<String> {
    let mut out = String::new();
    push_json_dumps(&mut out, value, false)?;
    Ok(out)
}

#[must_use]
pub fn format_sse_event(
    data: Option<&str>,
    event: Option<&str>,
    id: Option<&str>,
    retry: Option<i64>,
    comment: Option<&str>,
    sep: &str,
) -> bytes::Bytes {
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
        .getattr(pyo3::intern!(result.py(), "ping"))
        .ok()
        .and_then(|value| value.extract::<Option<f64>>().ok())
        .flatten();
    let sep = result
        .getattr(pyo3::intern!(result.py(), "sep"))
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
    let data = item
        .get_item(pyo3::intern!(item.py(), "data"))
        .ok()
        .flatten();
    let raw_data = item
        .get_item(pyo3::intern!(item.py(), "raw_data"))
        .ok()
        .flatten()
        .and_then(|value| value.extract().ok());
    let event = item
        .get_item(pyo3::intern!(item.py(), "event"))
        .ok()
        .flatten()
        .and_then(|value| value.extract().ok());
    let id = item
        .get_item(pyo3::intern!(item.py(), "id"))
        .ok()
        .flatten()
        .and_then(|value| value.extract().ok());
    let retry = item
        .get_item(pyo3::intern!(item.py(), "retry"))
        .ok()
        .flatten()
        .and_then(|value| value.extract().ok());
    let comment = item
        .get_item(pyo3::intern!(item.py(), "comment"))
        .ok()
        .flatten()
        .and_then(|value| value.extract().ok());
    Some((data, raw_data, event, id, retry, comment))
}
