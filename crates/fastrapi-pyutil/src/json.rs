//! Conversion between Python objects and JSON.
//! It writes Python values, Pydantic models, and dataclasses as JSON, and it makes axum JSON responses.
//! It also converts JSON values back into Python objects.

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response as AxumResponse};
use bytes::{BufMut, BytesMut};
use papaya::HashMap;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyMemoryView;
use pyo3::types::{
    PyAny, PyBool, PyByteArray, PyBytes, PyDict, PyFloat, PyFrozenSet, PyInt, PyList, PySet,
    PyString, PyTuple, PyType,
};
use pyo3::{Bound, intern};
use simd_json::owned::Object;
use simd_json::{OwnedValue as Value, StaticNode, owned};
use std::borrow::Cow;
use std::cell::RefCell;
use std::io::{self, Write};
use std::str::from_utf8;
use std::sync::LazyLock;

const KEY_CACHE_SLOTS: usize = 512;
const KEY_CACHE_MAX_LEN: usize = 64;

type KeySlot = Option<(Box<str>, Py<PyString>)>;

thread_local! {
    static RESPONSE_BUF: RefCell<BytesMut> = RefCell::new(BytesMut::with_capacity(1024));
    static KEY_CACHE: RefCell<Box<[KeySlot]>> =
        RefCell::new((0..KEY_CACHE_SLOTS).map(|_| None).collect());
}

fn key_slot(key: &str) -> usize {
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    (hash % KEY_CACHE_SLOTS as u64) as usize
}

#[must_use]
pub fn cached_key<'py>(py: Python<'py>, key: &str) -> Bound<'py, PyString> {
    if key.len() > KEY_CACHE_MAX_LEN {
        return PyString::new(py, key);
    }
    KEY_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let slot = &mut cache[key_slot(key)];
        if let Some((cached, value)) = slot.as_ref()
            && **cached == *key
        {
            return value.bind(py).clone();
        }
        let value = PyString::new(py, key);
        *slot = Some((key.into(), value.clone().unbind()));
        value
    })
}

crate::cached_py_import!(ENUM_TYPE, "enum", "Enum");
crate::cached_py_import!(DATACLASSES_ASDICT, "dataclasses", "asdict");
crate::cached_py_import!(GENERATOR_TYPE, "types", "GeneratorType");
crate::cached_py_import!(DEQUE_TYPE, "collections", "deque");
crate::cached_py_import!(DECIMAL_TYPE, "decimal", "Decimal");
crate::cached_py_import!(UUID_TYPE, "uuid", "UUID");
crate::cached_py_import!(PURE_PATH_TYPE, "pathlib", "PurePath");
crate::cached_py_import!(IP_ADDRESS_BASE, "ipaddress", "_IPAddressBase");

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SerializationHint {
    #[default]
    Unknown,
    PydanticModel,
    Dataclass,
    PlainDict,
}

#[inline]
pub fn is_enum_instance(py: Python<'_>, value: &Bound<'_, PyAny>) -> bool {
    ENUM_TYPE.is_instance(py, value)
}

fn json_text<'a>(text: &'a Bound<'_, PyString>) -> PyResult<Cow<'a, str>> {
    text.to_cow().map_or_else(
        |_| {
            Err(text
                .call_method1(intern!(text.py(), "encode"), ("utf-8",))
                .err()
                .unwrap_or_else(|| PyValueError::new_err("surrogates not allowed")))
        },
        Ok,
    )
}

fn json_text_of(value: &Bound<'_, PyAny>) -> PyResult<String> {
    json_text(&value.str()?).map(Cow::into_owned)
}

fn lossy_text(text: &Bound<'_, PyString>) -> String {
    json_text(text).map_or_else(|_| text.to_string_lossy().into_owned(), Cow::into_owned)
}

#[inline]
pub fn dataclasses_asdict(py: Python<'_>) -> Option<&Bound<'_, PyAny>> {
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
    py: Python<'_>,
    value: &Bound<'_, PyAny>,
    writer: &mut W,
    options: Option<&Bound<'_, PyDict>>,
) -> PyResult<bool> {
    let json = options.map_or_else(
        || value.call_method0(intern!(py, "model_dump_json")),
        |opts| value.call_method(intern!(py, "model_dump_json"), (), Some(opts)),
    );

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
#[must_use]
pub fn json_response(py: Python<'_>, value: &Value) -> AxumResponse {
    json_response_with_status(py, StatusCode::OK, value)
}

#[inline]
pub fn json_response_with_status(
    py: Python<'_>,
    status: StatusCode,
    value: &Value,
) -> AxumResponse {
    let json_bytes = py.detach(|| simd_json::to_vec(value).unwrap_or_default());

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
#[must_use]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Used as function pointer in Result::map_err"
)]
pub fn json_io_error(err: io::Error) -> PyErr {
    PyValueError::new_err(err.to_string())
}

#[inline]
#[must_use]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Used as function pointer in Result::map_err"
)]
pub fn json_ser_error(err: simd_json::Error) -> PyErr {
    PyValueError::new_err(err.to_string())
}

const ESCAPE: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut byte = 0;
    while byte < 0x20 {
        table[byte] = b'u';
        byte += 1;
    }
    table[0x08] = b'b';
    table[0x09] = b't';
    table[0x0A] = b'n';
    table[0x0C] = b'f';
    table[0x0D] = b'r';
    table[b'"' as usize] = b'"';
    table[b'\\' as usize] = b'\\';
    table
};

#[inline]
pub fn write_json_string<W: Write>(writer: &mut W, value: &str) -> PyResult<()> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    writer.write_all(b"\"").map_err(json_io_error)?;

    let bytes = value.as_bytes();
    let mut start = 0;

    for (i, &b) in bytes.iter().enumerate() {
        let code = ESCAPE[b as usize];
        if code == 0 {
            continue;
        }
        let unicode;
        let escape: &[u8] = if code == b'u' {
            unicode = [
                b'\\',
                b'u',
                b'0',
                b'0',
                HEX[(b >> 4) as usize],
                HEX[(b & 0x0F) as usize],
            ];
            &unicode
        } else {
            &[b'\\', code]
        };
        writer
            .write_all(&bytes[start..i])
            .and_then(|()| writer.write_all(escape))
            .map_err(json_io_error)?;
        start = i + 1;
    }

    writer
        .write_all(&bytes[start..])
        .and_then(|()| writer.write_all(b"\""))
        .map_err(json_io_error)
}

#[inline]
pub fn write_json_bytes<W: Write>(writer: &mut W, bytes: &[u8]) -> PyResult<()> {
    if let Ok(s) = from_utf8(bytes) {
        write_json_string(writer, s)
    } else {
        let mut scratch = Vec::with_capacity(bytes.len() * 4 + 2);
        scratch.push(b'[');
        for (idx, byte) in bytes.iter().enumerate() {
            if idx > 0 {
                scratch.push(b',');
            }
            _ = write!(scratch, "{byte}");
        }
        scratch.push(b']');
        writer.write_all(&scratch).map_err(json_io_error)
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
pub enum PyJsonKind<'a, 'py> {
    None,
    Dict(&'a Bound<'py, PyDict>),
    List(&'a Bound<'py, PyList>),
    Tuple(&'a Bound<'py, PyTuple>),
    Set(&'a Bound<'py, PySet>),
    FrozenSet(&'a Bound<'py, PyFrozenSet>),
    Str(&'a Bound<'py, PyString>),
    Bool(&'a Bound<'py, PyBool>),
    Int(&'a Bound<'py, PyInt>),
    Float(&'a Bound<'py, PyFloat>),
    Bytes(&'a Bound<'py, PyBytes>),
    ByteArray(&'a Bound<'py, PyByteArray>),
    MemoryView,
    PydanticModel,
    Dataclass,
    HasIsoformat,
    Timedelta,
    UuidOrDecimal,
    Decimal,
    Path,
    IpAddress,
    Enum,
    Iterable,
    Object,
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
    Decimal,
    Path,
    IpAddress,
    Iterable,
    StrSubclass,
    IntSubclass,
    FloatSubclass,
    Object,
    FallbackStr,
}

static OBJ_KIND_CACHE: LazyLock<papaya::HashMap<usize, (Py<PyType>, ObjKind)>> =
    LazyLock::new(|| HashMap::with_capacity(256));

#[inline]
fn classify_object_kind(py: Python<'_>, value: &Bound<'_, PyAny>) -> ObjKind {
    let ty = value.get_type();
    let type_ptr = ty.as_ptr() as usize;

    let guard = OBJ_KIND_CACHE.guard();
    if let Some((_, kind)) = OBJ_KIND_CACHE.get(&type_ptr, &guard) {
        return *kind;
    }
    drop(guard);

    let kind = probe_object_kind(py, value);
    OBJ_KIND_CACHE.pin().insert(type_ptr, (ty.unbind(), kind));
    kind
}

fn probe_object_kind(py: Python<'_>, value: &Bound<'_, PyAny>) -> ObjKind {
    let instance_of = |class: &crate::lazy::LazyPyAttr| class.is_instance(py, value);
    if instance_of(&GENERATOR_TYPE) || instance_of(&DEQUE_TYPE) {
        return ObjKind::Iterable;
    }

    if let Ok(has_model_dump) = value.hasattr(intern!(py, "model_dump_json"))
        && has_model_dump
    {
        return ObjKind::PydanticModel;
    }

    if let Ok(has_dataclass) = value.hasattr(intern!(py, "__dataclass_fields__"))
        && has_dataclass
    {
        return ObjKind::Dataclass;
    }

    if let Ok(has_isoformat) = value.hasattr(intern!(py, "isoformat"))
        && has_isoformat
    {
        return ObjKind::HasIsoformat;
    }

    if let Ok(total_seconds) = value.hasattr(intern!(py, "total_seconds"))
        && total_seconds
    {
        return ObjKind::Timedelta;
    }

    if is_enum_instance(py, value) {
        return ObjKind::Enum;
    }
    if value.is_instance_of::<PyString>() {
        return ObjKind::StrSubclass;
    }
    if value.is_instance_of::<PyInt>() {
        return ObjKind::IntSubclass;
    }
    if value.is_instance_of::<PyFloat>() {
        return ObjKind::FloatSubclass;
    }

    if instance_of(&DECIMAL_TYPE) {
        return ObjKind::Decimal;
    }
    if instance_of(&UUID_TYPE) {
        return ObjKind::UuidOrDecimal;
    }
    if instance_of(&PURE_PATH_TYPE) {
        return ObjKind::Path;
    }
    if instance_of(&IP_ADDRESS_BASE) {
        return ObjKind::IpAddress;
    }

    let is_pydantic_type = value
        .get_type()
        .module()
        .is_ok_and(|module| module.to_str().unwrap_or_default().starts_with("pydantic"));
    if is_pydantic_type {
        ObjKind::FallbackStr
    } else {
        ObjKind::Object
    }
}

fn object_fields<'py>(py: Python<'py>, value: &Bound<'py, PyAny>) -> Option<Bound<'py, PyAny>> {
    py.get_type::<PyDict>()
        .call1((value,))
        .or_else(|_| value.getattr(intern!(py, "__dict__")))
        .ok()
}

fn decimal_number<'py>(py: Python<'py>, value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let exponent = value
        .call_method0(intern!(py, "as_tuple"))?
        .getattr(intern!(py, "exponent"))?;
    let builtins = py.import(intern!(py, "builtins"))?;
    let target = if exponent
        .extract::<i64>()
        .is_ok_and(|exponent| exponent >= 0)
    {
        "int"
    } else {
        "float"
    };
    builtins.getattr(target)?.call1((value,))
}

#[inline]
#[must_use]
pub fn classify_py_value<'a, 'py>(
    py: Python<'py>,
    value: &'a Bound<'py, PyAny>,
) -> PyJsonKind<'a, 'py> {
    if value.is_none() {
        return PyJsonKind::None;
    }

    if let Ok(s) = value.cast_exact::<PyString>() {
        return PyJsonKind::Str(s);
    }
    if let Ok(i) = value.cast_exact::<PyInt>() {
        return PyJsonKind::Int(i);
    }
    if let Ok(b) = value.cast_exact::<PyBool>() {
        return PyJsonKind::Bool(b);
    }
    if let Ok(f) = value.cast_exact::<PyFloat>() {
        return PyJsonKind::Float(f);
    }
    if let Ok(d) = value.cast::<PyDict>() {
        return PyJsonKind::Dict(d);
    }
    if let Ok(l) = value.cast::<PyList>() {
        return PyJsonKind::List(l);
    }
    if let Ok(t) = value.cast::<PyTuple>() {
        return PyJsonKind::Tuple(t);
    }
    if let Ok(s) = value.cast::<PySet>() {
        return PyJsonKind::Set(s);
    }
    if let Ok(fs) = value.cast::<PyFrozenSet>() {
        return PyJsonKind::FrozenSet(fs);
    }
    if let Ok(b) = value.cast::<PyBytes>() {
        return PyJsonKind::Bytes(b);
    }
    if let Ok(b) = value.cast::<PyByteArray>() {
        return PyJsonKind::ByteArray(b);
    }

    if value.is_instance_of::<PyMemoryView>() {
        return PyJsonKind::MemoryView;
    }

    match classify_object_kind(py, value) {
        ObjKind::PydanticModel => PyJsonKind::PydanticModel,
        ObjKind::Dataclass => PyJsonKind::Dataclass,
        ObjKind::HasIsoformat => PyJsonKind::HasIsoformat,
        ObjKind::Timedelta => PyJsonKind::Timedelta,
        ObjKind::Enum => PyJsonKind::Enum,
        ObjKind::UuidOrDecimal => PyJsonKind::UuidOrDecimal,
        ObjKind::Decimal => PyJsonKind::Decimal,
        ObjKind::Path => PyJsonKind::Path,
        ObjKind::IpAddress => PyJsonKind::IpAddress,
        ObjKind::Iterable => PyJsonKind::Iterable,
        ObjKind::StrSubclass => value.cast::<PyString>().map_or(PyJsonKind::Object, PyJsonKind::Str),
        ObjKind::IntSubclass => value.cast::<PyInt>().map_or(PyJsonKind::Object, PyJsonKind::Int),
        ObjKind::FloatSubclass => value
            .cast::<PyFloat>()
            .map_or(PyJsonKind::Object, PyJsonKind::Float),
        ObjKind::Object => PyJsonKind::Object,
        ObjKind::FallbackStr => PyJsonKind::FallbackStr,
    }
}

const PLAIN_JSON_MAX_DEPTH: usize = 128;

fn write_plain_json<W: Write>(
    value: &Bound<'_, PyAny>,
    writer: &mut W,
    depth: usize,
) -> PyResult<bool> {
    if depth > PLAIN_JSON_MAX_DEPTH {
        return Ok(false);
    }
    if value.is_none() {
        writer.write_all(b"null").map_err(json_io_error)?;
    } else if let Ok(text) = value.cast_exact::<PyString>() {
        let Ok(text) = text.to_str() else {
            return Ok(false);
        };
        write_json_string(writer, text)?;
    } else if let Ok(flag) = value.cast_exact::<PyBool>() {
        let literal: &[u8] = if flag.is_true() { b"true" } else { b"false" };
        writer.write_all(literal).map_err(json_io_error)?;
    } else if let Ok(number) = value.cast_exact::<PyInt>() {
        match number.extract::<i64>() {
            Ok(number) => writer
                .write_all(itoa::Buffer::new().format(number).as_bytes())
                .map_err(json_io_error)?,
            Err(_) => writer
                .write_all(number.str()?.to_str()?.as_bytes())
                .map_err(json_io_error)?,
        }
    } else if let Ok(number) = value.cast_exact::<PyFloat>() {
        if !number.value().is_finite() {
            return Ok(false);
        }
        writer
            .write_all(number.repr()?.to_str()?.as_bytes())
            .map_err(json_io_error)?;
    } else if let Ok(dict) = value.cast_exact::<PyDict>() {
        writer.write_all(b"{").map_err(json_io_error)?;
        for (index, (key, item)) in dict.iter().enumerate() {
            let Ok(key) = key.cast_exact::<PyString>() else {
                return Ok(false);
            };
            let Ok(key) = key.to_str() else {
                return Ok(false);
            };
            if index > 0 {
                writer.write_all(b",").map_err(json_io_error)?;
            }
            write_json_string(writer, key)?;
            writer.write_all(b":").map_err(json_io_error)?;
            if !write_plain_json(&item, writer, depth + 1)? {
                return Ok(false);
            }
        }
        writer.write_all(b"}").map_err(json_io_error)?;
    } else if let Ok(list) = value.cast_exact::<PyList>() {
        return write_plain_array(list.iter(), writer, depth);
    } else if let Ok(tuple) = value.cast_exact::<PyTuple>() {
        return write_plain_array(tuple.iter(), writer, depth);
    } else {
        return Ok(false);
    }
    Ok(true)
}

fn write_plain_array<'py, W: Write>(
    items: impl Iterator<Item = Bound<'py, PyAny>>,
    writer: &mut W,
    depth: usize,
) -> PyResult<bool> {
    writer.write_all(b"[").map_err(json_io_error)?;
    for (index, item) in items.enumerate() {
        if index > 0 {
            writer.write_all(b",").map_err(json_io_error)?;
        }
        if !write_plain_json(&item, writer, depth + 1)? {
            return Ok(false);
        }
    }
    writer.write_all(b"]").map_err(json_io_error)?;
    Ok(true)
}

pub fn plain_json_bytes<'py>(value: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyBytes>>> {
    let mut supported = false;
    let rendered = PyBytes::new_with_writer(value.py(), 64, |mut writer| {
        supported = write_plain_json(value, &mut writer, 0)?;
        Ok(())
    })?;
    Ok(supported.then_some(rendered))
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

        PyJsonKind::Str(s) => write_json_string(writer, &json_text(s)?),

        PyJsonKind::Bool(b) => {
            if b.is_true() {
                writer.write_all(b"true").map_err(json_io_error)
            } else {
                writer.write_all(b"false").map_err(json_io_error)
            }
        }

        PyJsonKind::Int(i) => {
            if let Ok(v) = i.extract::<i64>() {
                writer
                    .write_all(itoa::Buffer::new().format(v).as_bytes())
                    .map_err(json_io_error)
            } else if let Ok(s) = json_text_of(value) {
                writer.write_all(s.as_bytes()).map_err(json_io_error)
            } else {
                writer.write_all(b"0").map_err(json_io_error)
            }
        }

        PyJsonKind::Float(f) => match f.extract::<f64>() {
            Ok(v) if v.is_finite() => write!(writer, "{v:?}").map_err(json_io_error),
            Ok(_) => Err(PyValueError::new_err(
                "Out of range float values are not JSON compliant",
            )),
            Err(_) => writer.write_all(b"0.0").map_err(json_io_error),
        },

        PyJsonKind::Bytes(b) => write_json_bytes(writer, b.as_bytes()),

        PyJsonKind::ByteArray(b) => {
            let bytes = b.to_vec();
            write_json_bytes(writer, &bytes)
        }

        PyJsonKind::MemoryView => {
            if let Ok(b) = value.call_method0(intern!(py, "tobytes"))
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

            if let Ok(dumped) = value.call_method0(intern!(py, "model_dump")) {
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
            if let Ok(obj) = value.call_method0(intern!(py, "isoformat"))
                && let Ok(s) = obj.cast::<PyString>()
            {
                return write_json_string(writer, &json_text(s)?);
            }

            writer.write_all(b"null").map_err(json_io_error)
        }

        PyJsonKind::Timedelta => {
            if let Ok(total) = value.call_method0(intern!(py, "total_seconds"))
                && let Ok(f) = total.extract::<f64>()
            {
                return write!(writer, "{f}").map_err(json_io_error);
            }

            if let Ok(s) = json_text_of(value) {
                write_json_string(writer, &s)
            } else {
                writer.write_all(b"null").map_err(json_io_error)
            }
        }

        PyJsonKind::Iterable => {
            let items = value
                .try_iter()?
                .collect::<PyResult<Vec<Bound<'_, PyAny>>>>()?;
            write_json_array(py, writer, items)
        }

        PyJsonKind::Object => match object_fields(py, value) {
            Some(fields) => write_py_json(py, &fields, writer),
            None => match json_text_of(value) {
                Ok(s) => write_json_string(writer, &s),
                Err(_) => writer.write_all(b"null").map_err(json_io_error),
            },
        },

        PyJsonKind::Decimal => match decimal_number(py, value) {
            Ok(number) => write_py_json(py, &number, writer),
            Err(_) => writer.write_all(b"null").map_err(json_io_error),
        },

        PyJsonKind::UuidOrDecimal
        | PyJsonKind::Path
        | PyJsonKind::IpAddress
        | PyJsonKind::FallbackStr => {
            if let Ok(s) = json_text_of(value) {
                write_json_string(writer, &s)
            } else {
                writer.write_all(b"null").map_err(json_io_error)
            }
        }

        PyJsonKind::Enum => {
            if let Ok(inner) = value.getattr(intern!(py, "value")) {
                write_py_json(py, &inner, writer)
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

pub fn local_guard<K, V, S>(map: &papaya::HashMap<K, V, S>) -> papaya::LocalGuard<'_> {
    map.guard()
}

pub fn owned_guard<K, V, S>(map: &papaya::HashMap<K, V, S>) -> papaya::OwnedGuard<'_> {
    map.owned_guard()
}

/// Converts a JSON value into a Python object.
#[inline]
#[must_use]
pub fn json_to_py_object(py: Python<'_>, value: &Value) -> Py<PyAny> {
    json_value_to_py(py, value).map_or_else(|_| py.None(), Bound::unbind)
}

pub fn json_value_to_py<'py>(py: Python<'py>, value: &Value) -> PyResult<Bound<'py, PyAny>> {
    Ok(match value {
        Value::Static(StaticNode::Null) => py.None().into_bound(py),
        Value::Static(StaticNode::Bool(flag)) => PyBool::new(py, *flag).to_owned().into_any(),
        Value::Static(StaticNode::I64(number)) => number.into_pyobject(py)?.into_any(),
        Value::Static(StaticNode::U64(number)) => number.into_pyobject(py)?.into_any(),
        Value::Static(StaticNode::F64(number)) => PyFloat::new(py, *number).into_any(),
        Value::String(text) => PyString::new(py, text).into_any(),
        Value::Array(items) => {
            let list = PyList::empty(py);
            for item in items.iter() {
                list.append(json_value_to_py(py, item)?)?;
            }
            list.into_any()
        }
        Value::Object(entries) => {
            let dict = PyDict::new(py);
            for (key, item) in entries.iter() {
                dict.set_item(cached_key(py, key), json_value_to_py(py, item)?)?;
            }
            dict.into_any()
        }
    })
}

#[inline]
#[must_use]
pub fn py_to_response(py: Python<'_>, obj: &Bound<'_, PyAny>, status: StatusCode) -> AxumResponse {
    py_json_response_with_status(py, status, obj).unwrap_or_else(|err| {
        err.print(py);
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })
}

#[inline]
#[must_use]
pub fn py_dict_to_json(py: Python<'_>, dict: &Bound<'_, PyDict>) -> Value {
    let mut map = Object::new();

    dict.iter().for_each(|(key, value)| {
        let k = json_key_for(&key);
        map.insert(k, py_any_to_json(py, &value));
    });

    owned::Value::Object(Box::new(map))
}

#[inline]
#[must_use]
pub fn py_list_to_json(py: Python<'_>, list: &Bound<'_, PyList>) -> Value {
    let mut vec = Vec::with_capacity(list.len());

    vec.extend(list.iter().map(|item| py_any_to_json(py, &item)));

    Value::Array(Box::new(vec))
}

#[inline]
#[must_use]
pub fn json_key_for(key: &Bound<'_, PyAny>) -> String {
    py_dict_key_str(key).into_owned()
}

#[inline]
fn py_dict_key_str<'k>(key: &'k Bound<'_, PyAny>) -> Cow<'k, str> {
    if let Ok(s) = key.cast::<PyString>() {
        return match s.to_str() {
            Ok(s) => Cow::Borrowed(s),
            Err(_) => Cow::Owned(String::new()),
        };
    }
    key.str()
        .map(|s| Cow::Owned(s.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

#[inline]
pub fn py_any_to_json(py: Python<'_>, value: &Bound<'_, PyAny>) -> Value {
    match classify_py_value(py, value) {
        PyJsonKind::None => simd_json::json!(null),
        PyJsonKind::Dict(dict) => py_dict_to_json(py, dict),
        PyJsonKind::List(list) => py_list_to_json(py, list),
        PyJsonKind::Str(s) => simd_json::json!(lossy_text(s)),
        PyJsonKind::Bool(b) => simd_json::json!(b.is_true()),
        PyJsonKind::Int(i) => i.extract::<i64>().map_or_else(
            |_| {
                if let Ok(s) = value.str() {
                    simd_json::json!(lossy_text(&s))
                } else {
                    simd_json::json!(null)
                }
            },
            |v| simd_json::json!(v),
        ),

        PyJsonKind::Float(f) => match f.extract::<f64>() {
            Ok(v) if v.is_finite() => simd_json::json!(v),
            _ => simd_json::json!(null),
        },

        PyJsonKind::Tuple(tuple) => {
            let mut vec = Vec::with_capacity(tuple.len());
            vec.extend(tuple.iter().map(|item| py_any_to_json(py, &item)));
            Value::Array(Box::new(vec))
        }
        PyJsonKind::Set(set) => {
            let mut vec = Vec::with_capacity(set.len());
            vec.extend(set.iter().map(|item| py_any_to_json(py, &item)));
            Value::Array(Box::new(vec))
        }
        PyJsonKind::FrozenSet(fset) => {
            let mut vec = Vec::with_capacity(fset.len());
            vec.extend(fset.iter().map(|item| py_any_to_json(py, &item)));
            Value::Array(Box::new(vec))
        }

        PyJsonKind::Bytes(b) => bytes_to_json(b.as_bytes()),
        PyJsonKind::ByteArray(b) => {
            let bytes = b.to_vec();
            bytes_to_json(&bytes)
        }
        PyJsonKind::MemoryView => {
            if let Ok(b) = value.call_method0(intern!(py, "tobytes"))
                && let Ok(bytes) = b.cast::<PyBytes>()
            {
                return bytes_to_json(bytes.as_bytes());
            }
            simd_json::json!(null)
        }

        PyJsonKind::PydanticModel => {
            if let Ok(json) = value.call_method0(intern!(py, "model_dump_json"))
                && let Ok(s) = json.cast::<PyString>()
            {
                let mut json_buf = s.to_str().unwrap_or_default().as_bytes().to_vec();
                if let Ok(parsed) = simd_json::to_owned_value(&mut json_buf) {
                    return parsed;
                }
            }
            if let Ok(dumped) = value.call_method0(intern!(py, "model_dump")) {
                return py_any_to_json(py, &dumped);
            }
            simd_json::json!(null)
        }

        PyJsonKind::Dataclass => {
            if let Some(asdict) = dataclasses_asdict(py)
                && let Ok(d) = asdict.call1((value,))
            {
                return py_any_to_json(py, &d);
            }
            simd_json::json!(null)
        }

        PyJsonKind::HasIsoformat => {
            if let Ok(obj) = value.call_method0(intern!(py, "isoformat"))
                && let Ok(s) = obj.cast::<PyString>()
            {
                return simd_json::json!(lossy_text(s));
            }

            simd_json::json!(null)
        }

        PyJsonKind::Timedelta => {
            if let Ok(total) = value.call_method0(intern!(py, "total_seconds"))
                && let Ok(f) = total.extract::<f64>()
            {
                return simd_json::json!(f);
            }
            value.str().map_or_else(
                |_| simd_json::json!(null),
                |s| simd_json::json!(lossy_text(&s)),
            )
        }

        PyJsonKind::Iterable => value.try_iter().map_or_else(
            |_| simd_json::json!(null),
            |iter| {
                Value::Array(Box::new(
                    iter.filter_map(Result::ok)
                        .map(|item| py_any_to_json(py, &item))
                        .collect(),
                ))
            },
        ),

        PyJsonKind::Object => object_fields(py, value).map_or_else(
            || {
                value.str().map_or_else(
                    |_| simd_json::json!(null),
                    |s| simd_json::json!(lossy_text(&s)),
                )
            },
            |fields| py_any_to_json(py, &fields),
        ),

        PyJsonKind::Decimal => decimal_number(py, value).map_or_else(
            |_| simd_json::json!(null),
            |number| py_any_to_json(py, &number),
        ),

        PyJsonKind::UuidOrDecimal
        | PyJsonKind::Path
        | PyJsonKind::IpAddress
        | PyJsonKind::FallbackStr => value.str().map_or_else(
            |_| simd_json::json!(null),
            |s| simd_json::json!(lossy_text(&s)),
        ),

        PyJsonKind::Enum => {
            if let Ok(inner) = value.getattr(intern!(py, "value")) {
                py_any_to_json(py, &inner)
            } else {
                simd_json::json!(null)
            }
        }
    }
}

#[inline]
#[must_use]
pub fn bytes_to_json(b: &[u8]) -> Value {
    from_utf8(b).map_or_else(
        |_| {
            Value::Array(Box::new(
                b.iter().map(|&x| simd_json::json!(x)).collect::<Vec<_>>(),
            ))
        },
        |s| simd_json::json!(s.to_owned()),
    )
}
