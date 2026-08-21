use crate::routing::types::SerializationHint;
use axum::{
    body::Body,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response as AxumResponse},
};
use bytes::{BufMut, BytesMut};
use pyo3::{
    exceptions::PyValueError,
    intern,
    prelude::*,
    types::{
        PyAny, PyBool, PyByteArray, PyBytes, PyDict, PyFloat, PyFrozenSet, PyInt, PyList, PySet,
        PyString, PyTuple,
    },
};
use pythonize::pythonize;
use sonic_rs::Value;
use std::{
    cell::RefCell,
    io::{self, Write},
};

thread_local! {
    static RESPONSE_BUF: RefCell<BytesMut> = RefCell::new(BytesMut::with_capacity(1024));
}

crate::cached_py_import!(ENUM_TYPE, "enum", "Enum");
crate::cached_py_import!(DATACLASSES_ASDICT, "dataclasses", "asdict");

#[inline]
pub fn is_enum_instance(py: Python<'_>, value: &Bound<'_, PyAny>) -> bool {
    ENUM_TYPE.is_instance(py, value)
}

#[inline]
pub fn dataclasses_asdict(py: Python<'_>) -> Option<Bound<'_, PyAny>> {
    DATACLASSES_ASDICT.get(py).ok()
}

#[inline]
pub fn write_pydantic_model_json<W: Write>(
    py: Python<'_>,
    value: &Bound<'_, PyAny>,
    writer: &mut W,
) -> PyResult<bool> {
    write_pydantic_model_json_opts(py, value, writer, None)
}

#[inline]
pub fn write_pydantic_model_json_opts<W: Write>(
    _py: Python<'_>,
    value: &Bound<'_, PyAny>,
    writer: &mut W,
    options: Option<&Bound<'_, PyDict>>,
) -> PyResult<bool> {
    let json = match options {
        Some(opts) => value.call_method("model_dump_json", (), Some(opts)),
        None => value.call_method0("model_dump_json"),
    };

    let Ok(json) = json else {
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
pub fn write_dataclass_json<W: Write>(
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

#[inline]
pub fn json_response(py: Python<'_>, value: &Value) -> AxumResponse {
    json_response_with_status(py, StatusCode::OK, value)
}

#[inline]
pub fn json_response_with_status(
    py: Python<'_>,
    status: StatusCode,
    value: &Value,
) -> AxumResponse {
    let json_bytes = py.detach(|| sonic_rs::to_vec(value).unwrap_or_default());

    AxumResponse::builder()
        .status(status)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )
        .body(Body::from(json_bytes))
        .expect("Failed to build validation response")
}

#[inline]
pub fn json_io_error(err: io::Error) -> PyErr {
    PyValueError::new_err(err.to_string())
}

#[inline]
pub fn json_ser_error(err: sonic_rs::Error) -> PyErr {
    PyValueError::new_err(err.to_string())
}

#[inline]
pub fn write_json_string<W: Write>(writer: &mut W, value: &str) -> PyResult<()> {
    writer.write_all(b"\"").map_err(json_io_error)?;

    let bytes = value.as_bytes();
    let mut start = 0;

    for (i, &b) in bytes.iter().enumerate() {
        let escape: &[u8] = match b {
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            0x08 => b"\\b",
            0x09 => b"\\t",
            0x0A => b"\\n",
            0x0C => b"\\f",
            0x0D => b"\\r",
            0..=0x1F => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let buf = [
                    b'\\',
                    b'u',
                    b'0',
                    b'0',
                    HEX[(b >> 4) as usize],
                    HEX[(b & 0x0F) as usize],
                ];
                writer
                    .write_all(&bytes[start..i])
                    .and_then(|_| writer.write_all(&buf))
                    .map_err(json_io_error)?;
                start = i + 1;
                continue;
            }
            _ => continue,
        };

        writer
            .write_all(&bytes[start..i])
            .and_then(|_| writer.write_all(escape))
            .map_err(json_io_error)?;
        start = i + 1;
    }

    writer
        .write_all(&bytes[start..])
        .and_then(|_| writer.write_all(b"\""))
        .map_err(json_io_error)
}

#[inline]
pub fn write_json_bytes<W: Write>(writer: &mut W, bytes: &[u8]) -> PyResult<()> {
    match std::str::from_utf8(bytes) {
        Ok(s) => write_json_string(writer, s),
        Err(_) => {
            let mut scratch = Vec::with_capacity(bytes.len() * 4 + 2);
            scratch.push(b'[');
            for (idx, byte) in bytes.iter().enumerate() {
                if idx > 0 {
                    scratch.push(b',');
                }
                let _ = write!(scratch, "{byte}");
            }
            scratch.push(b']');
            writer.write_all(&scratch).map_err(json_io_error)
        }
    }
}

#[inline]
pub fn write_json_array<'py, W, I>(py: Python<'py>, writer: &mut W, items: I) -> PyResult<()>
where
    W: Write,
    I: IntoIterator<Item = Bound<'py, PyAny>>,
{
    writer.write_all(b"[").map_err(json_io_error)?;
    let mut iter = items.into_iter();
    if let Some(first_item) = iter.next() {
        write_py_json(py, &first_item, writer)?;
        iter.try_for_each(|item| {
            writer.write_all(b",").map_err(json_io_error)?;
            write_py_json(py, &item, writer)
        })?;
    }

    writer.write_all(b"]").map_err(json_io_error)
}

#[derive(Debug)]
pub enum PyJsonKind<'py> {
    None,
    Dict(Bound<'py, PyDict>),
    List(Bound<'py, PyList>),
    Tuple(Bound<'py, PyTuple>),
    Set(Bound<'py, PySet>),
    FrozenSet(Bound<'py, PyFrozenSet>),
    Str(Bound<'py, PyString>),
    Bool(Bound<'py, PyBool>),
    Int(Bound<'py, PyInt>),
    Float(Bound<'py, PyFloat>),
    Bytes(Bound<'py, PyBytes>),
    ByteArray(Bound<'py, PyByteArray>),
    MemoryView,
    PydanticModel,
    Dataclass,
    HasIsoformat, // date / time / datetime
    Timedelta,
    UuidOrDecimal,
    Path,
    IpAddress,
    Enum,
    FallbackStr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObjKind {
    PydanticModel,
    Dataclass,
    HasIsoformat,
    Timedelta,
    Enum,
    UuidOrDecimal,
    Path,
    IpAddress,
    FallbackStr,
}

static OBJ_KIND_CACHE: std::sync::LazyLock<papaya::HashMap<usize, ObjKind>> =
    std::sync::LazyLock::new(|| papaya::HashMap::with_capacity(256));

#[inline]
fn classify_object_kind(py: Python<'_>, value: &Bound<'_, PyAny>) -> ObjKind {
    let type_ptr = value.get_type().as_ptr() as usize;

    let guard = OBJ_KIND_CACHE.guard();
    if let Some(kind) = OBJ_KIND_CACHE.get(&type_ptr, &guard) {
        return *kind;
    }
    drop(guard);

    let kind = probe_object_kind(py, value);
    OBJ_KIND_CACHE.pin().insert(type_ptr, kind);
    kind
}

fn probe_object_kind(py: Python<'_>, value: &Bound<'_, PyAny>) -> ObjKind {
    if let Ok(has_model_dump) = value.hasattr(intern!(py, "model_dump_json"))
        && has_model_dump
    {
        return ObjKind::PydanticModel;
    }

    if let Ok(has_dataclass) = value.hasattr("__dataclass_fields__")
        && has_dataclass
    {
        return ObjKind::Dataclass;
    }

    if let Ok(has_isoformat) = value.hasattr("isoformat")
        && has_isoformat
    {
        return ObjKind::HasIsoformat;
    }

    if let Ok(total_seconds) = value.hasattr("total_seconds")
        && total_seconds
    {
        return ObjKind::Timedelta;
    }

    if is_enum_instance(py, value) {
        return ObjKind::Enum;
    }

    if let Ok(type_obj) = value.get_type().name() {
        let type_name = type_obj.to_str().unwrap_or_default();
        if type_name == "UUID" || type_name == "Decimal" {
            return ObjKind::UuidOrDecimal;
        }
        if type_name.ends_with("Path") {
            return ObjKind::Path;
        }
        if type_name.starts_with("IPv") {
            return ObjKind::IpAddress;
        }
    }

    ObjKind::FallbackStr
}

#[inline]
pub fn classify_py_value<'py>(py: Python<'py>, value: &Bound<'py, PyAny>) -> PyJsonKind<'py> {
    if value.is_none() {
        return PyJsonKind::None;
    }

    if let Ok(d) = value.cast::<PyDict>() {
        return PyJsonKind::Dict(d.clone());
    }
    if let Ok(l) = value.cast::<PyList>() {
        return PyJsonKind::List(l.clone());
    }
    if let Ok(t) = value.cast::<PyTuple>() {
        return PyJsonKind::Tuple(t.clone());
    }
    if let Ok(s) = value.cast::<PySet>() {
        return PyJsonKind::Set(s.clone());
    }
    if let Ok(fs) = value.cast::<PyFrozenSet>() {
        return PyJsonKind::FrozenSet(fs.clone());
    }
    if let Ok(s) = value.cast::<PyString>() {
        return PyJsonKind::Str(s.clone());
    }
    if let Ok(b) = value.cast::<PyBool>() {
        return PyJsonKind::Bool(b.clone());
    }
    if let Ok(i) = value.cast::<PyInt>() {
        return PyJsonKind::Int(i.clone());
    }
    if let Ok(f) = value.cast::<PyFloat>() {
        return PyJsonKind::Float(f.clone());
    }
    if let Ok(b) = value.cast::<PyBytes>() {
        return PyJsonKind::Bytes(b.clone());
    }
    if let Ok(b) = value.cast::<PyByteArray>() {
        return PyJsonKind::ByteArray(b.clone());
    }

    if value.is_instance_of::<pyo3::types::PyMemoryView>() {
        return PyJsonKind::MemoryView;
    }

    match classify_object_kind(py, value) {
        ObjKind::PydanticModel => PyJsonKind::PydanticModel,
        ObjKind::Dataclass => PyJsonKind::Dataclass,
        ObjKind::HasIsoformat => PyJsonKind::HasIsoformat,
        ObjKind::Timedelta => PyJsonKind::Timedelta,
        ObjKind::Enum => PyJsonKind::Enum,
        ObjKind::UuidOrDecimal => PyJsonKind::UuidOrDecimal,
        ObjKind::Path => PyJsonKind::Path,
        ObjKind::IpAddress => PyJsonKind::IpAddress,
        ObjKind::FallbackStr => PyJsonKind::FallbackStr,
    }
}

#[inline]
pub fn write_py_json<W: Write>(
    py: Python<'_>,
    value: &Bound<'_, PyAny>,
    writer: &mut W,
) -> PyResult<()> {
    match classify_py_value(py, value) {
        PyJsonKind::None => writer.write_all(b"null").map_err(json_io_error),

        PyJsonKind::Dict(dict) => {
            writer.write_all(b"{").map_err(json_io_error)?;
            let mut iter = dict.iter();
            if let Some((first_key, first_val)) = iter.next() {
                write_json_string(writer, &py_dict_key_str(&first_key))?;
                writer.write_all(b":").map_err(json_io_error)?;
                write_py_json(py, &first_val, writer)?;

                iter.try_for_each(|(key, val)| {
                    writer.write_all(b",").map_err(json_io_error)?;
                    write_json_string(writer, &py_dict_key_str(&key))?;
                    writer.write_all(b":").map_err(json_io_error)?;
                    write_py_json(py, &val, writer)
                })?;
            }
            writer.write_all(b"}").map_err(json_io_error)
        }

        PyJsonKind::List(list) => write_json_array(py, writer, list.iter()),

        PyJsonKind::Tuple(tuple) => write_json_array(py, writer, tuple.iter()),

        PyJsonKind::Set(set) => write_json_array(py, writer, set.iter()),

        PyJsonKind::FrozenSet(fset) => write_json_array(py, writer, fset.iter()),

        PyJsonKind::Str(s) => write_json_string(writer, s.to_str().unwrap_or_default()),

        PyJsonKind::Bool(b) => {
            if b.is_true() {
                writer.write_all(b"true").map_err(json_io_error)
            } else {
                writer.write_all(b"false").map_err(json_io_error)
            }
        }

        PyJsonKind::Int(i) => {
            if let Ok(v) = i.extract::<i64>() {
                write!(writer, "{v}").map_err(json_io_error)
            } else if let Ok(s) = value.str().and_then(|s| s.to_str().map(str::to_owned)) {
                writer.write_all(s.as_bytes()).map_err(json_io_error)
            } else {
                writer.write_all(b"0").map_err(json_io_error)
            }
        }

        PyJsonKind::Float(f) => {
            if let Ok(v) = f.extract::<f64>() {
                write!(writer, "{v}").map_err(json_io_error)
            } else {
                writer.write_all(b"0.0").map_err(json_io_error)
            }
        }

        PyJsonKind::Bytes(b) => write_json_bytes(writer, b.as_bytes()),

        PyJsonKind::ByteArray(b) => write_json_bytes(writer, unsafe { b.as_bytes() }),

        PyJsonKind::MemoryView => {
            if let Ok(b) = value.call_method0("tobytes")
                && let Ok(bytes) = b.cast::<PyBytes>()
            {
                return write_json_bytes(writer, bytes.as_bytes());
            }
            writer.write_all(b"null").map_err(json_io_error)
        }

        PyJsonKind::PydanticModel => {
            if write_pydantic_model_json(py, value, writer)? {
                return Ok(());
            }

            if let Ok(dumped) = value.call_method0("model_dump") {
                return write_py_json(py, &dumped, writer);
            }

            writer.write_all(b"null").map_err(json_io_error)
        }

        PyJsonKind::Dataclass => {
            if write_dataclass_json(py, value, writer)? {
                return Ok(());
            }

            writer.write_all(b"null").map_err(json_io_error)
        }

        PyJsonKind::HasIsoformat => {
            if let Ok(obj) = value.call_method0("isoformat")
                && let Ok(s) = obj.cast::<PyString>()
            {
                return write_json_string(writer, s.to_str().unwrap_or_default());
            }

            writer.write_all(b"null").map_err(json_io_error)
        }

        PyJsonKind::Timedelta => {
            if let Ok(total) = value.call_method0("total_seconds")
                && let Ok(f) = total.extract::<f64>()
            {
                return write!(writer, "{f}").map_err(json_io_error);
            }

            if let Ok(s) = value.str().and_then(|s| s.to_str().map(str::to_owned)) {
                write_json_string(writer, &s)
            } else {
                writer.write_all(b"null").map_err(json_io_error)
            }
        }

        PyJsonKind::UuidOrDecimal | PyJsonKind::Path | PyJsonKind::IpAddress => {
            if let Ok(s) = value.str().and_then(|s| s.to_str().map(str::to_owned)) {
                write_json_string(writer, &s)
            } else {
                writer.write_all(b"null").map_err(json_io_error)
            }
        }

        PyJsonKind::Enum => {
            if let Ok(inner) = value.getattr("value") {
                write_py_json(py, &inner, writer)
            } else {
                writer.write_all(b"null").map_err(json_io_error)
            }
        }

        PyJsonKind::FallbackStr => {
            if let Ok(s) = value.str().and_then(|s| s.to_str().map(str::to_owned)) {
                write_json_string(writer, &s)
            } else {
                writer.write_all(b"null").map_err(json_io_error)
            }
        }
    }
}

#[inline]
pub fn py_json_response_with_status(
    py: Python<'_>,
    status: StatusCode,
    value: &Bound<'_, PyAny>,
) -> PyResult<AxumResponse> {
    py_json_response_with_status_hint(py, status, value, SerializationHint::Unknown)
}

#[inline]
pub fn py_json_response_with_status_hint(
    py: Python<'_>,
    status: StatusCode,
    value: &Bound<'_, PyAny>,
    hint: SerializationHint,
) -> PyResult<AxumResponse> {
    py_json_response_with_dump_options(py, status, value, hint, None)
}

#[inline]
pub fn py_json_response_with_dump_options(
    py: Python<'_>,
    status: StatusCode,
    value: &Bound<'_, PyAny>,
    hint: SerializationHint,
    dump_options: Option<&Bound<'_, PyDict>>,
) -> PyResult<AxumResponse> {
    let bytes = RESPONSE_BUF.with(|cell| {
        let mut buf = cell.take();
        buf.clear();

        let write_result = {
            let mut writer = (&mut buf).writer();
            match hint {
                SerializationHint::PydanticModel => {
                    if write_pydantic_model_json_opts(py, value, &mut writer, dump_options)? {
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

    Ok(AxumResponse::builder()
        .status(status)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )
        .body(Body::from(bytes))
        .expect("Failed to build response"))
}

#[inline]
pub fn py_json_response(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<AxumResponse> {
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
#[inline]
pub fn json_to_py_object(py: Python<'_>, value: &Value) -> Py<PyAny> {
    pythonize(py, value)
        .map(|obj| obj.unbind())
        .unwrap_or_else(|_| py.None())
}

#[inline]
pub fn py_to_response(py: Python<'_>, obj: &Bound<'_, PyAny>, status: StatusCode) -> AxumResponse {
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

    sonic_rs::to_value(&map).unwrap_or_else(|_| sonic_rs::json!(null))
}

#[inline]
pub fn py_list_to_json(py: Python<'_>, list: &Bound<'_, PyList>) -> Value {
    let mut vec = Vec::with_capacity(list.len());

    vec.extend(list.iter().map(|item| py_any_to_json(py, &item)));

    sonic_rs::to_value(&vec).unwrap_or_else(|_| sonic_rs::json!(null))
}

#[inline]
pub fn json_key_for(key: &Bound<'_, PyAny>) -> String {
    py_dict_key_str(key).into_owned()
}

#[inline]
fn py_dict_key_str<'k>(key: &'k Bound<'_, PyAny>) -> std::borrow::Cow<'k, str> {
    if let Ok(s) = key.cast::<PyString>() {
        return match s.to_str() {
            Ok(s) => std::borrow::Cow::Borrowed(s),
            Err(_) => std::borrow::Cow::Owned(String::new()),
        };
    }
    key.str()
        .map(|s| std::borrow::Cow::Owned(s.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

/// walk a Python value into a sonic_rs::Value.
#[inline]
pub fn py_any_to_json(py: Python<'_>, value: &Bound<'_, PyAny>) -> Value {
    match classify_py_value(py, value) {
        PyJsonKind::None => sonic_rs::json!(null),
        PyJsonKind::Dict(dict) => py_dict_to_json(py, &dict),
        PyJsonKind::List(list) => py_list_to_json(py, &list),
        PyJsonKind::Str(s) => sonic_rs::json!(s.to_str().unwrap_or_default().to_owned()),
        PyJsonKind::Bool(b) => sonic_rs::json!(b.is_true()),
        PyJsonKind::Int(i) => {
            if let Ok(v) = i.extract::<i64>() {
                sonic_rs::json!(v)
            } else if let Ok(s) = value.str().and_then(|s| s.to_str().map(str::to_owned)) {
                sonic_rs::json!(s)
            } else {
                sonic_rs::json!(null)
            }
        }

        PyJsonKind::Float(f) => {
            if let Ok(v) = f.extract::<f64>() {
                sonic_rs::json!(v)
            } else {
                sonic_rs::json!(null)
            }
        }

        PyJsonKind::Tuple(tuple) => {
            let mut vec = Vec::with_capacity(tuple.len());
            vec.extend(tuple.iter().map(|item| py_any_to_json(py, &item)));
            sonic_rs::to_value(&vec).unwrap_or_else(|_| sonic_rs::json!(null))
        }
        PyJsonKind::Set(set) => {
            let mut vec = Vec::with_capacity(set.len());
            vec.extend(set.iter().map(|item| py_any_to_json(py, &item)));
            sonic_rs::to_value(&vec).unwrap_or_else(|_| sonic_rs::json!(null))
        }
        PyJsonKind::FrozenSet(fset) => {
            let mut vec = Vec::with_capacity(fset.len());
            vec.extend(fset.iter().map(|item| py_any_to_json(py, &item)));
            sonic_rs::to_value(&vec).unwrap_or_else(|_| sonic_rs::json!(null))
        }

        PyJsonKind::Bytes(b) => bytes_to_json(b.as_bytes()),
        PyJsonKind::ByteArray(b) => bytes_to_json(unsafe { b.as_bytes() }),
        PyJsonKind::MemoryView => {
            if let Ok(b) = value.call_method0("tobytes")
                && let Ok(bytes) = b.cast::<PyBytes>()
            {
                return bytes_to_json(bytes.as_bytes());
            }
            sonic_rs::json!(null)
        }

        PyJsonKind::PydanticModel => {
            if let Ok(json) = value.call_method0(intern!(py, "model_dump_json"))
                && let Ok(s) = json.cast::<PyString>()
                && let Ok(parsed) = sonic_rs::from_str(s.to_str().unwrap_or_default())
            {
                return parsed;
            }
            if let Ok(dumped) = value.call_method0("model_dump") {
                return py_any_to_json(py, &dumped);
            }
            sonic_rs::json!(null)
        }

        PyJsonKind::Dataclass => {
            if let Some(asdict) = dataclasses_asdict(py)
                && let Ok(d) = asdict.call1((value,))
            {
                return py_any_to_json(py, &d);
            }
            sonic_rs::json!(null)
        }

        PyJsonKind::HasIsoformat => {
            if let Ok(obj) = value.call_method0("isoformat")
                && let Ok(s) = obj.cast::<PyString>()
            {
                return sonic_rs::json!(s.to_str().unwrap_or_default().to_owned());
            }

            sonic_rs::json!(null)
        }

        PyJsonKind::Timedelta => {
            if let Ok(total) = value.call_method0("total_seconds")
                && let Ok(f) = total.extract::<f64>()
            {
                return sonic_rs::json!(f);
            }
            if let Ok(s) = value.str().and_then(|s| s.to_str().map(str::to_owned)) {
                sonic_rs::json!(s)
            } else {
                sonic_rs::json!(null)
            }
        }

        PyJsonKind::UuidOrDecimal | PyJsonKind::Path | PyJsonKind::IpAddress => {
            if let Ok(s) = value.str().and_then(|s| s.to_str().map(str::to_owned)) {
                sonic_rs::json!(s)
            } else {
                sonic_rs::json!(null)
            }
        }

        PyJsonKind::Enum => {
            if let Ok(inner) = value.getattr("value") {
                py_any_to_json(py, &inner)
            } else {
                sonic_rs::json!(null)
            }
        }

        PyJsonKind::FallbackStr => {
            if let Ok(s) = value.str().and_then(|s| s.to_str().map(str::to_owned)) {
                sonic_rs::json!(s)
            } else {
                sonic_rs::json!(null)
            }
        }
    }
}

#[inline]
pub fn bytes_to_json(b: &[u8]) -> Value {
    match std::str::from_utf8(b) {
        Ok(s) => sonic_rs::json!(s.to_owned()),
        Err(_) => sonic_rs::to_value(&b.iter().map(|&x| sonic_rs::json!(x)).collect::<Vec<_>>())
            .unwrap_or_else(|_| sonic_rs::json!(null)),
    }
}
