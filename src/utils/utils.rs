use crate::routing::types::SerializationHint;
use axum::{
    body::Body,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::{BufMut, BytesMut};
use pyo3::types::{
    PyAny, PyBool, PyByteArray, PyBytes, PyDict, PyFloat, PyFrozenSet, PyInt, PyList, PySet,
    PyTuple,
};
use pyo3::{exceptions::PyValueError, intern, prelude::*, types::PyString};
use sonic_rs::Value;
use std::sync::OnceLock;
use std::{
    cell::RefCell,
    io::{self, Write},
};

thread_local! {
    /// Per-thread JSON response buffer. `sonic_rs::to_writer` writes here,
    /// then we `split()` off the written portion as immutable `Bytes` (zero
    /// copy) and feed it directly into the response body. The cell keeps the
    /// underlying allocation across requests so steady-state response building
    /// pays no buffer alloc.
    static RESPONSE_BUF: RefCell<BytesMut> = RefCell::new(BytesMut::with_capacity(1024));
}

static ENUM_TYPE: OnceLock<Py<PyAny>> = OnceLock::new();
static DATACLASSES_ASDICT: OnceLock<Py<PyAny>> = OnceLock::new();

#[inline]
fn is_enum_instance(py: Python<'_>, value: &Bound<'_, PyAny>) -> bool {
    let enum_type =
        ENUM_TYPE.get_or_init(|| py.import("enum").unwrap().getattr("Enum").unwrap().unbind());

    value.is_instance(enum_type.bind(py)).unwrap_or(false)
}

#[inline]
fn dataclasses_asdict(py: Python<'_>) -> Option<Bound<'_, PyAny>> {
    let func = DATACLASSES_ASDICT.get_or_init(|| {
        py.import(intern!(py, "dataclasses"))
            .unwrap()
            .getattr(intern!(py, "asdict"))
            .unwrap()
            .unbind()
    });
    Some(func.bind(py).clone())
}

#[inline]
fn write_pydantic_model_json<W: Write>(
    py: Python<'_>,
    value: &Bound<'_, PyAny>,
    writer: &mut W,
) -> PyResult<bool> {
    let Ok(json) = value.call_method0(intern!(py, "model_dump_json")) else {
        return Ok(false);
    };

    if let Ok(s) = json.cast::<PyString>() {
        writer
            .write_all(s.to_str().unwrap_or_default().as_bytes())
            .map_err(json_io_error)?;
        return Ok(true);
    }

    if let Ok(bytes) = json.cast::<PyBytes>() {
        writer.write_all(bytes.as_bytes()).map_err(json_io_error)?;
        return Ok(true);
    }

    Ok(false)
}

#[inline]
fn write_dataclass_json<W: Write>(
    py: Python<'_>,
    value: &Bound<'_, PyAny>,
    writer: &mut W,
) -> PyResult<bool> {
    let Some(asdict) = dataclasses_asdict(py) else {
        return Ok(false);
    };
    let Ok(asdict) = asdict.call1((value,)) else {
        return Ok(false);
    };
    write_py_json(py, &asdict, writer)?;
    Ok(true)
}

/// Serialize a `sonic_rs::Value` into an axum JSON response, reusing a
/// per-thread `BytesMut` buffer.
#[inline]
pub fn json_response(py: Python<'_>, value: &Value) -> Response {
    json_response_with_status(py, StatusCode::OK, value)
}

#[inline]
pub fn json_response_with_status(py: Python<'_>, status: StatusCode, value: &Value) -> Response {
    // ponytail: sonic_rs::to_vec is the lazy path — no WriteExt trait needed
    let json_bytes = py.detach(|| sonic_rs::to_vec(value).unwrap_or_default());

    Response::builder()
        .status(status)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )
        .body(Body::from(json_bytes))
        .unwrap()
}

#[inline]
fn json_io_error(err: io::Error) -> PyErr {
    PyValueError::new_err(err.to_string())
}

#[inline]
fn json_ser_error(err: sonic_rs::Error) -> PyErr {
    PyValueError::new_err(err.to_string())
}

// ponytail: helper wrapping sonic_rs::to_string for write paths
#[inline]
fn write_json_via_sonic<W: Write>(writer: &mut W, value: &str) -> PyResult<()> {
    writer.write_all(
        sonic_rs::to_string(&value)
            .map_err(json_ser_error)?
            .as_bytes(),
    ).map_err(json_io_error)
}

#[inline]
fn write_json_string<W: Write>(writer: &mut W, value: &str) -> PyResult<()> {
    write_json_via_sonic(writer, value)
}

#[inline]
fn write_json_bytes<W: Write>(writer: &mut W, bytes: &[u8]) -> PyResult<()> {
    match std::str::from_utf8(bytes) {
        Ok(s) => write_json_string(writer, s),
        Err(_) => {
            writer.write_all(b"[").map_err(json_io_error)?;
            for (idx, byte) in bytes.iter().enumerate() {
                if idx > 0 {
                    writer.write_all(b",").map_err(json_io_error)?;
                }
                write!(writer, "{byte}").map_err(json_io_error)?;
            }
            writer.write_all(b"]").map_err(json_io_error)
        }
    }
}

#[inline]
fn write_json_array<'py, W, I>(py: Python<'py>, writer: &mut W, items: I) -> PyResult<()>
where
    W: Write,
    I: IntoIterator<Item = Bound<'py, PyAny>>,
{
    writer.write_all(b"[").map_err(json_io_error)?;
    let mut first = true;
    for item in items {
        if !first {
            writer.write_all(b",").map_err(json_io_error)?;
        }
        first = false;
        write_py_json(py, &item, writer)?;
    }
    writer.write_all(b"]").map_err(json_io_error)
}

#[inline]
fn write_py_json<W: Write>(
    py: Python<'_>,
    value: &Bound<'_, PyAny>,
    writer: &mut W,
) -> PyResult<()> {
    if value.is_none() {
        return writer.write_all(b"null").map_err(json_io_error);
    }

    if let Ok(dict) = value.cast::<PyDict>() {
        writer.write_all(b"{").map_err(json_io_error)?;
        let mut first = true;
        for (key, item) in dict.iter() {
            if !first {
                writer.write_all(b",").map_err(json_io_error)?;
            }
            first = false;
            write_json_string(writer, &json_key_for(&key))?;
            writer.write_all(b":").map_err(json_io_error)?;
            write_py_json(py, &item, writer)?;
        }
        return writer.write_all(b"}").map_err(json_io_error);
    }

    if let Ok(list) = value.cast::<PyList>() {
        return write_json_array(py, writer, list.iter());
    }

    if let Ok(tuple) = value.cast::<PyTuple>() {
        return write_json_array(py, writer, tuple.iter());
    }

    if let Ok(set) = value.cast::<PySet>() {
        return write_json_array(py, writer, set.iter());
    }

    if let Ok(fset) = value.cast::<PyFrozenSet>() {
        return write_json_array(py, writer, fset.iter());
    }

    if let Ok(s) = value.cast::<PyString>() {
        return write_json_string(writer, s.to_str().unwrap_or_default());
    }

    if let Ok(b) = value.cast::<PyBool>() {
        return writer
            .write_all(if b.is_true() { b"true" } else { b"false" })
            .map_err(json_io_error);
    }

    if let Ok(i) = value.cast::<PyInt>() {
        if let Ok(v) = i.extract::<i64>() {
            write!(writer, "{v}").map_err(json_io_error)?;
            return Ok(());
        }

        if let Ok(s) = value.str()
            && let Ok(s) = s.to_str()
        {
            return write_json_string(writer, s);
        }
    }

    if let Ok(f) = value.cast::<PyFloat>() {
        if let Some(number) = sonic_rs::Number::from_f64(f.value()) {
            write!(writer, "{number}").map_err(json_io_error)?;
        } else {
            writer.write_all(b"null").map_err(json_io_error)?;
        }
        return Ok(());
    }

    if let Ok(b) = value.cast::<PyBytes>() {
        return write_json_bytes(writer, b.as_bytes());
    }

    if let Ok(b) = value.cast::<PyByteArray>() {
        let snapshot = b.to_vec();
        return write_json_bytes(writer, &snapshot);
    }

    if let Ok(true) = value.hasattr("model_dump_json")
        && write_pydantic_model_json(py, value, writer)?
    {
        return Ok(());
    }

    if let Ok(true) = value.hasattr("model_dump")
        && let Ok(dumped) = value.call_method0("model_dump")
    {
        return write_py_json(py, &dumped, writer);
    }

    if let Ok(true) = value.hasattr("__dataclass_fields__")
        && let Some(asdict) = dataclasses_asdict(py)
        && let Ok(asdict) = asdict.call1((value,))
    {
        return write_py_json(py, &asdict, writer);
    }

    if let Ok(true) = value.hasattr("isoformat")
        && let Ok(s) = value.call_method0("isoformat")
        && let Ok(s) = s.cast_into::<PyString>()
    {
        return write_json_string(writer, s.to_str().unwrap_or_default());
    }

    if let Ok(class_name) = value
        .get_type()
        .name()
        .map(|n| n.to_str().unwrap_or_default().to_owned())
    {
        match class_name.as_str() {
            "UUID" | "Decimal" => {
                if let Ok(s) = value.str()
                    && let Ok(s) = s.to_str()
                {
                    return write_json_string(writer, s);
                }
            }
            _ => {}
        }
    }

    if is_enum_instance(py, value)
        && let Ok(inner) = value.getattr("value")
    {
        return write_py_json(py, &inner, writer);
    }

    if let Ok(s) = value.str()
        && let Ok(s) = s.to_str()
    {
        return write_json_string(writer, s);
    }

    writer.write_all(b"null").map_err(json_io_error)
}

#[inline]
pub fn py_json_response_with_status(
    py: Python<'_>,
    status: StatusCode,
    value: &Bound<'_, PyAny>,
) -> PyResult<Response> {
    py_json_response_with_status_hint(py, status, value, SerializationHint::Unknown)
}

#[inline]
pub fn py_json_response_with_status_hint(
    py: Python<'_>,
    status: StatusCode,
    value: &Bound<'_, PyAny>,
    hint: SerializationHint,
) -> PyResult<Response> {
    let bytes = RESPONSE_BUF.with(|cell| {
        let mut buf = cell.take();
        buf.clear();

        let write_result = {
            let mut writer = (&mut buf).writer();
            match hint {
                SerializationHint::PydanticModel => {
                    if write_pydantic_model_json(py, value, &mut writer)? {
                        Ok(())
                    } else {
                        write_py_json(py, value, &mut writer)
                    }
                }
                SerializationHint::Dataclass => {
                    if write_dataclass_json(py, value, &mut writer)? {
                        Ok(())
                    } else {
                        write_py_json(py, value, &mut writer)
                    }
                }
                SerializationHint::PlainDict | SerializationHint::Unknown => {
                    write_py_json(py, value, &mut writer)
                }
            }
        };

        match write_result {
            Ok(()) => {
                let bytes = buf.split().freeze();
                cell.replace(buf);
                Ok(bytes)
            }
            Err(err) => {
                cell.replace(buf);
                Err(err)
            }
        }
    })?;

    Ok(Response::builder()
        .status(status)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )
        .body(Body::from(bytes))
        .unwrap())
}

#[inline]
pub fn py_json_response(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<Response> {
    py_json_response_with_status(py, StatusCode::OK, value)
}

// for local reads (fast, non-Send for sync blocks like spawn_blocking)
pub fn local_guard<K, V, S>(map: &papaya::HashMap<K, V, S>) -> papaya::LocalGuard<'_> {
    map.guard()
}

// for async/Send (in handlers)
pub fn owned_guard<K, V, S>(map: &papaya::HashMap<K, V, S>) -> papaya::OwnedGuard<'_> {
    map.owned_guard()
}

/// Fast JSON to Python conversion
/// ponytail: serialize to string then let Python json.loads handle it — avoids serde_pyobject's serde_json::Value dependency
#[inline]
pub fn json_to_py_object(py: Python<'_>, value: &Value) -> Py<PyAny> {
    let json_str = sonic_rs::to_string(value).unwrap_or_else(|_| "null".to_owned());
    static JSON_LOADS: OnceLock<Py<PyAny>> = OnceLock::new();
    let loads = JSON_LOADS.get_or_init(|| {
        py.import("json").unwrap().getattr("loads").unwrap().unbind()
    });
    loads.bind(py).call1((&json_str,))
        .map(|obj| obj.unbind())
        .unwrap_or_else(|_| py.None())
}

#[inline]
pub fn py_to_response(py: Python<'_>, obj: &Bound<'_, PyAny>, status: StatusCode) -> Response {
    if obj.is_none() {
        let final_status = if status == StatusCode::OK {
            StatusCode::NO_CONTENT
        } else {
            status
        };
        return final_status.into_response();
    }

    py_json_response_with_status(py, status, obj).unwrap_or_else(|err| {
        err.print(py);
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })
}
#[inline]
pub fn py_dict_to_json(py: Python<'_>, dict: &Bound<'_, PyDict>) -> Value {
    let mut map = std::collections::HashMap::with_capacity(dict.len());

    dict.iter().for_each(|(key, value)| {
        let k = json_key_for(&key);
        map.insert(k, py_any_to_json(py, &value));
    });

    sonic_rs::to_value(&map).unwrap()
}

#[inline]
pub fn py_list_to_json(py: Python<'_>, list: &Bound<'_, PyList>) -> Value {
    let mut vec = Vec::with_capacity(list.len());

    vec.extend(list.iter().map(|item| py_any_to_json(py, &item)));

    sonic_rs::to_value(&vec).unwrap()
}

/// Coerce any Python dict key into a JSON-acceptable string.
/// JSON object keys must be strings :  for non-string keys we use repr-style
/// `str(key)`, matching Python `json.dumps` with `default=str` behavior on
/// non-string keys (after `sort_keys`/`skipkeys` are off).
#[inline]
fn json_key_for(key: &Bound<'_, PyAny>) -> String {
    if let Ok(s) = key.cast::<PyString>() {
        return s.to_str().unwrap_or_default().to_owned();
    }
    key.str()
        .ok()
        .and_then(|s| s.to_str().ok().map(|s| s.to_owned()))
        .unwrap_or_default()
}

/// Walk an arbitrary Python value into a sonic_rs::Value.
///
/// Type ladder (most common first; bool before int because `True/False` are
/// `int` subclasses in Python):
///   None, dict, list, str, bool, int, float,
///   tuple, set/frozenset,
///   bytes/bytearray (utf8 if valid, otherwise list of byte ints),
///   datetime/date/time (isoformat),
///   UUID, Decimal,
///   Enum (recurse on `.value`),
///   pydantic BaseModel (`.model_dump()` then recurse),
///   dataclass (`dataclasses.asdict()` then recurse),
///   fallback: `str(value)`.
#[inline]
pub fn py_any_to_json(py: Python<'_>, value: &Bound<'_, PyAny>) -> Value {
    if value.is_none() {
        return sonic_rs::json!(null);
    }

    if let Ok(dict) = value.cast::<PyDict>() {
        return py_dict_to_json(py, dict);
    }
    if let Ok(list) = value.cast::<PyList>() {
        return py_list_to_json(py, list);
    }
    if let Ok(s) = value.cast::<PyString>() {
        return sonic_rs::json!(s.to_str().unwrap_or_default().to_owned());
    }
    if let Ok(b) = value.cast::<PyBool>() {
        return sonic_rs::json!(b.is_true());
    }
    if let Ok(i) = value.cast::<PyInt>() {
        if let Ok(v) = i.extract::<i64>() {
            return sonic_rs::json!(v);
        }
        // bigints that don't fit in i64: fall back to string to preserve precision.
        if let Ok(s) = value.str()
            && let Ok(s) = s.to_str()
        {
            return sonic_rs::json!(s.to_owned());
        }
    }
    if let Ok(f) = value.cast::<PyFloat>()
        && let Ok(v) = f.extract::<f64>()
    {
        return sonic_rs::json!(v);
    }

    if let Ok(tuple) = value.cast::<PyTuple>() {
        let mut vec = Vec::with_capacity(tuple.len());
        vec.extend(tuple.iter().map(|item| py_any_to_json(py, &item)));

        return sonic_rs::to_value(&vec).unwrap();
    }
    if let Ok(set) = value.cast::<PySet>() {
        let mut vec = Vec::with_capacity(set.len());
        vec.extend(set.iter().map(|item| py_any_to_json(py, &item)));

        return sonic_rs::to_value(&vec).unwrap();
    }
    if let Ok(fset) = value.cast::<PyFrozenSet>() {
        let mut vec = Vec::with_capacity(fset.len());
        vec.extend(fset.iter().map(|item| py_any_to_json(py, &item)));

        return sonic_rs::to_value(&vec).unwrap();
    }

    if let Ok(b) = value.cast::<PyBytes>() {
        return bytes_to_json(b.as_bytes());
    }
    if let Ok(b) = value.cast::<PyByteArray>() {
        let snapshot = b.to_vec();
        return bytes_to_json(&snapshot);
    }

    // Duck-typed conversions. Order matters: pydantic BaseModel exposes
    // `model_dump`; dataclasses expose `__dataclass_fields__`; Enum exposes
    // `value`; datetime/date/time/UUID/Decimal are matched by class name to
    // avoid importing each module on the hot path.
    if let Ok(true) = value.hasattr("model_dump_json")
        && let Ok(json) = value.call_method0(intern!(py, "model_dump_json"))
        && let Ok(s) = json.cast::<PyString>()
        && let Ok(parsed) = sonic_rs::from_str(s.to_str().unwrap_or_default())
    {
        return parsed;
    }

    if let Ok(true) = value.hasattr("model_dump")
        && let Ok(dumped) = value.call_method0("model_dump")
    {
        return py_any_to_json(py, &dumped);
    }

    if let Ok(true) = value.hasattr("__dataclass_fields__")
        && let Some(asdict) = dataclasses_asdict(py)
        && let Ok(asdict) = asdict.call1((value,))
    {
        return py_any_to_json(py, &asdict);
    }

    if let Ok(true) = value.hasattr("isoformat")
        && let Ok(s) = value.call_method0("isoformat")
        && let Ok(s) = s.cast_into::<PyString>()
    {
        return sonic_rs::json!(s.to_str().unwrap_or_default().to_owned());
    }

    if let Ok(class_name) = value
        .get_type()
        .name()
        .map(|n| n.to_str().unwrap_or_default().to_owned())
    {
        match class_name.as_str() {
            "UUID" => {
                if let Ok(s) = value.str()
                    && let Ok(s) = s.to_str()
                {
                    return sonic_rs::json!(s.to_owned());
                }
            }
            "Decimal" => {
                if let Ok(s) = value.str()
                    && let Ok(s) = s.to_str()
                {
                    return sonic_rs::json!(s.to_owned());
                }
            }
            _ => {}
        }
    }
    if is_enum_instance(py, value)
        && let Ok(inner) = value.getattr("value")
    {
        return py_any_to_json(py, &inner);
    }

    if let Ok(s) = value.str()
        && let Ok(s) = s.to_str()
    {
        return sonic_rs::json!(s.to_owned());
    }

    sonic_rs::json!(null)
}

#[inline]
fn bytes_to_json(b: &[u8]) -> Value {
    match std::str::from_utf8(b) {
        Ok(s) => sonic_rs::json!(s.to_owned()),
        Err(_) => {
            sonic_rs::to_value(&b.iter().map(|&x| sonic_rs::json!(x)).collect::<Vec<_>>()).unwrap()
        }
    }
}
