//! Conversion between Python objects and JSON.
//! It writes Python values, Pydantic models, and dataclasses as JSON, and it makes axum JSON responses.
//! It also converts JSON values back into Python objects.

use crate::type_cache::TypeCache;
use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response as AxumResponse;
use bytes::{BufMut, BytesMut};
use pyo3::exceptions::PyAttributeError;
use pyo3::exceptions::PyRecursionError;
use pyo3::exceptions::PyTypeError;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyMemoryView;
use pyo3::types::{
    PyAny, PyBool, PyByteArray, PyBytes, PyDict, PyFloat, PyFrozenSet, PyInt, PyList, PySet,
    PyString, PyTuple,
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

static BY_ALIAS: pyo3::sync::PyOnceLock<Py<PyDict>> = pyo3::sync::PyOnceLock::new();

fn by_alias(py: Python<'_>) -> Option<&Bound<'_, PyDict>> {
    BY_ALIAS
        .get_or_try_init(py, || {
            let options = PyDict::new(py);
            options.set_item(intern!(py, "by_alias"), true)?;
            Ok::<_, PyErr>(options.unbind())
        })
        .ok()
        .map(|options| options.bind(py))
}

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
    write_pydantic_model_json_opts(py, value, writer, by_alias(py))
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

    let json = match json {
        Ok(json) => json,
        // A route hinted as a model can still return a plain value; that is a fast-path miss.
        Err(err)
            if err.is_instance_of::<PyAttributeError>(py)
                && !value
                    .hasattr(intern!(py, "model_dump_json"))
                    .unwrap_or(false) =>
        {
            return Ok(false);
        }
        Err(err) => return Err(err),
    };

    if let Ok(s) = json.cast::<PyString>() {
        writer
            .write_all(json_text(s)?.as_bytes())
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
    write_py_json_at(py, &asdict, writer, 0)?;
    Ok(true)
}

/// The deepest nesting `write_py_json` and `py_any_to_json` follow before they stop.
/// It keeps a cyclic value from overflowing the native stack.
const MAX_JSON_DEPTH: usize = 256;

#[cold]
fn json_depth_error() -> PyErr {
    PyRecursionError::new_err("maximum recursion depth exceeded while encoding a JSON object")
}

/// Writes a float the way Python's `float.__repr__` does, which is what `json.dumps` emits.
/// Non-finite values raise the `ValueError` that `json.dumps(allow_nan=False)` raises.
#[inline]
pub fn write_json_float<W: Write>(writer: &mut W, value: f64) -> PyResult<()> {
    if !value.is_finite() {
        return Err(non_finite_float_error(value));
    }
    let mut buf = [0u8; 40];
    let len = {
        let mut cursor = io::Cursor::new(&mut buf[..]);
        write!(cursor, "{value:?}").map_err(json_io_error)?;
        cursor.position() as usize
    };
    let text = &buf[..len];
    let Some(e) = text.iter().position(|&b| b == b'e') else {
        return writer.write_all(text).map_err(json_io_error);
    };
    // Rust prints `1e16` and `1e-7`; Python prints `1e+16` and `1e-07`.
    let (sign, digits) = match text[e + 1..].split_first() {
        Some((b'-', digits)) => (b'-', digits),
        _ => (b'+', &text[e + 1..]),
    };
    writer
        .write_all(&text[..=e])
        .and_then(|()| writer.write_all(&[sign]))
        .and_then(|()| {
            if digits.len() < 2 {
                writer.write_all(b"0")
            } else {
                Ok(())
            }
        })
        .and_then(|()| writer.write_all(digits))
        .map_err(json_io_error)
}

#[cold]
fn non_finite_float_error(value: f64) -> PyErr {
    let repr = if value.is_nan() {
        "nan"
    } else if value > 0.0 {
        "inf"
    } else {
        "-inf"
    };
    PyValueError::new_err(format!(
        "Out of range float values are not JSON compliant: {repr}"
    ))
}

fn float_text(value: f64) -> PyResult<String> {
    let mut out = Vec::with_capacity(24);
    write_json_float(&mut out, value)?;
    Ok(String::from_utf8(out).unwrap_or_default())
}

/// The digits `json.dumps` writes for an int: `int.__repr__`, so subclasses cannot override it.
fn int_text(py: Python<'_>, number: &Bound<'_, PyInt>) -> PyResult<String> {
    if let Ok(small) = number.extract::<i64>() {
        return Ok(itoa::Buffer::new().format(small).to_owned());
    }
    big_int_text(py, number)
}

#[cold]
fn big_int_text(py: Python<'_>, number: &Bound<'_, PyInt>) -> PyResult<String> {
    let repr = py
        .get_type::<PyInt>()
        .getattr(intern!(py, "__repr__"))?
        .call1((number,))?;
    Ok(json_text(repr.cast::<PyString>()?)?.into_owned())
}

/// The text of a dict key, as `jsonable_encoder` encodes the key and `json.dumps` then writes it.
fn json_key_text(py: Python<'_>, key: &Bound<'_, PyAny>, depth: usize) -> PyResult<String> {
    if depth > MAX_JSON_DEPTH {
        return Err(json_depth_error());
    }
    match classify_py_value(py, key) {
        PyJsonKind::Str(text) => Ok(json_text(text)?.into_owned()),
        PyJsonKind::None => Ok("null".to_owned()),
        PyJsonKind::Bool(flag) => Ok(if flag.is_true() { "true" } else { "false" }.to_owned()),
        PyJsonKind::Int(number) => int_text(py, number),
        PyJsonKind::Float(number) => float_text(number.value()),
        PyJsonKind::Bytes(bytes) => py
            .get_type::<PyBytes>()
            .getattr(intern!(py, "decode"))?
            .call1((bytes,))
            .and_then(|text| Ok(json_text(text.cast::<PyString>()?)?.into_owned())),
        PyJsonKind::Enum => json_key_text(py, &key.getattr(intern!(py, "value"))?, depth + 1),
        PyJsonKind::Decimal => json_key_text(py, &decimal_number(py, key)?, depth + 1),
        PyJsonKind::Timedelta => float_text(
            key.call_method0(intern!(py, "total_seconds"))?
                .extract::<f64>()?,
        ),
        PyJsonKind::HasIsoformat => {
            let text = key.call_method0(intern!(py, "isoformat"))?;
            Ok(json_text(text.cast::<PyString>()?)?.into_owned())
        }
        PyJsonKind::UuidOrDecimal
        | PyJsonKind::Path
        | PyJsonKind::IpAddress
        | PyJsonKind::FallbackStr => json_text_of(key),
        _ => Err(PyTypeError::new_err(format!(
            "keys must be str, int, float, bool or None, not {}",
            key.get_type().name()?
        ))),
    }
}

#[inline]
fn write_json_null<W: Write>(writer: &mut W) -> PyResult<()> {
    writer.write_all(b"null").map_err(json_io_error)
}

#[inline]
fn write_json_bool<W: Write>(writer: &mut W, flag: bool) -> PyResult<()> {
    let literal: &[u8] = if flag { b"true" } else { b"false" };
    writer.write_all(literal).map_err(json_io_error)
}

#[inline]
fn write_json_int<W: Write>(writer: &mut W, number: &Bound<'_, PyInt>) -> PyResult<()> {
    match number.extract::<i64>() {
        Ok(small) => writer
            .write_all(itoa::Buffer::new().format(small).as_bytes())
            .map_err(json_io_error),
        Err(_) => writer
            .write_all(big_int_text(number.py(), number)?.as_bytes())
            .map_err(json_io_error),
    }
}

/// Writes `str(value)`, or `null` when the value has no usable text.
fn write_json_text_or_null<W: Write>(writer: &mut W, value: &Bound<'_, PyAny>) -> PyResult<()> {
    match json_text_of(value) {
        Ok(text) => write_json_string(writer, &text),
        Err(_) => write_json_null(writer),
    }
}

#[inline]
fn write_json_key<W: Write>(
    py: Python<'_>,
    key: &Bound<'_, PyAny>,
    writer: &mut W,
    depth: usize,
) -> PyResult<()> {
    if let Ok(text) = key.cast_exact::<PyString>() {
        return write_json_string(writer, &json_text(text)?);
    }
    write_json_string(writer, &json_key_text(py, key, depth)?)
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
    write_json_array_at(py, writer, items, 0)
}

#[inline]
fn write_json_array_at<'py, W, I>(
    py: Python<'py>,
    writer: &mut W,
    items: I,
    depth: usize,
) -> PyResult<()>
where
    W: Write,
    I: IntoIterator<Item = Bound<'py, PyAny>>,
{
    writer.write_all(b"[").map_err(json_io_error)?;
    let mut iter = items.into_iter();
    if let Some(first_item) = iter.next() {
        write_py_json_at(py, &first_item, writer, depth + 1)?;
        iter.try_for_each(|item| {
            writer.write_all(b",").map_err(json_io_error)?;
            write_py_json_at(py, &item, writer, depth + 1)
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

static OBJ_KIND_CACHE: LazyLock<TypeCache<ObjKind>> = LazyLock::new(|| TypeCache::new(256, 1024));

#[inline]
fn classify_object_kind(py: Python<'_>, value: &Bound<'_, PyAny>) -> ObjKind {
    OBJ_KIND_CACHE.get_or_probe(value.get_type(), |_| probe_object_kind(py, value))
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
        ObjKind::StrSubclass => value
            .cast::<PyString>()
            .map_or(PyJsonKind::Object, PyJsonKind::Str),
        ObjKind::IntSubclass => value
            .cast::<PyInt>()
            .map_or(PyJsonKind::Object, PyJsonKind::Int),
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
        write_json_null(writer)?;
    } else if let Ok(text) = value.cast_exact::<PyString>() {
        let Ok(text) = text.to_str() else {
            return Ok(false);
        };
        write_json_string(writer, text)?;
    } else if let Ok(flag) = value.cast_exact::<PyBool>() {
        write_json_bool(writer, flag.is_true())?;
    } else if let Ok(number) = value.cast_exact::<PyInt>() {
        write_json_int(writer, number)?;
    } else if let Ok(number) = value.cast_exact::<PyFloat>() {
        if !number.value().is_finite() {
            return Ok(false);
        }
        write_json_float(writer, number.value())?;
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
    write_py_json_at(py, value, writer, 0)
}

fn write_py_json_at<W: Write>(
    py: Python<'_>,
    value: &Bound<'_, PyAny>,
    writer: &mut W,
    depth: usize,
) -> PyResult<()> {
    if depth > MAX_JSON_DEPTH {
        return Err(json_depth_error());
    }
    match classify_py_value(py, value) {
        PyJsonKind::None => write_json_null(writer),

        PyJsonKind::Dict(dict) => {
            writer.write_all(b"{").map_err(json_io_error)?;
            let mut iter = dict.iter();
            if let Some((first_key, first_val)) = iter.next() {
                write_json_key(py, &first_key, writer, depth + 1)?;
                writer.write_all(b":").map_err(json_io_error)?;
                write_py_json_at(py, &first_val, writer, depth + 1)?;

                iter.try_for_each(|(key, val)| {
                    writer.write_all(b",").map_err(json_io_error)?;
                    write_json_key(py, &key, writer, depth + 1)?;
                    writer.write_all(b":").map_err(json_io_error)?;
                    write_py_json_at(py, &val, writer, depth + 1)
                })?;
            }
            writer.write_all(b"}").map_err(json_io_error)
        }

        PyJsonKind::List(list) => write_json_array_at(py, writer, list.iter(), depth),

        PyJsonKind::Tuple(tuple) => write_json_array_at(py, writer, tuple.iter(), depth),

        PyJsonKind::Set(set) => write_json_array_at(py, writer, set.iter(), depth),

        PyJsonKind::FrozenSet(fset) => write_json_array_at(py, writer, fset.iter(), depth),

        PyJsonKind::Str(s) => write_json_string(writer, &json_text(s)?),

        PyJsonKind::Bool(b) => write_json_bool(writer, b.is_true()),

        PyJsonKind::Int(i) => write_json_int(writer, i),

        PyJsonKind::Float(f) => write_json_float(writer, f.value()),

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
            write_json_null(writer)
        }

        PyJsonKind::PydanticModel => {
            if write_pydantic_model_json(py, value, writer)? {
                return Ok(());
            }

            // `model_dump_json` returned something other than text; dump to Python and encode that.
            let dumped = value.call_method(intern!(py, "model_dump"), (), by_alias(py))?;
            write_py_json_at(py, &dumped, writer, depth + 1)
        }

        PyJsonKind::Dataclass => {
            let Some(asdict) = dataclasses_asdict(py) else {
                return write_json_null(writer);
            };
            write_py_json_at(py, &asdict.call1((value,))?, writer, depth + 1)
        }

        PyJsonKind::HasIsoformat => {
            if let Ok(obj) = value.call_method0(intern!(py, "isoformat"))
                && let Ok(s) = obj.cast::<PyString>()
            {
                return write_json_string(writer, &json_text(s)?);
            }

            write_json_null(writer)
        }

        PyJsonKind::Timedelta => {
            if let Ok(total) = value.call_method0(intern!(py, "total_seconds"))
                && let Ok(f) = total.extract::<f64>()
            {
                return write_json_float(writer, f);
            }

            write_json_text_or_null(writer, value)
        }

        PyJsonKind::Iterable => {
            let items = value
                .try_iter()?
                .collect::<PyResult<Vec<Bound<'_, PyAny>>>>()?;
            write_json_array_at(py, writer, items, depth)
        }

        PyJsonKind::Object => match object_fields(py, value) {
            Some(fields) => write_py_json_at(py, &fields, writer, depth + 1),
            None => write_json_text_or_null(writer, value),
        },

        PyJsonKind::Decimal => match decimal_number(py, value) {
            Ok(number) => write_py_json_at(py, &number, writer, depth + 1),
            Err(_) => write_json_null(writer),
        },

        PyJsonKind::UuidOrDecimal
        | PyJsonKind::Path
        | PyJsonKind::IpAddress
        | PyJsonKind::FallbackStr => write_json_text_or_null(writer, value),

        PyJsonKind::Enum => {
            if let Ok(inner) = value.getattr(intern!(py, "value")) {
                write_py_json_at(py, &inner, writer, depth + 1)
            } else {
                write_json_null(writer)
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
pub fn py_dict_to_json(py: Python<'_>, dict: &Bound<'_, PyDict>) -> Value {
    py_dict_to_json_at(py, dict, 0)
}

fn py_dict_to_json_at(py: Python<'_>, dict: &Bound<'_, PyDict>, depth: usize) -> Value {
    let mut map = Object::new();

    dict.iter().for_each(|(key, value)| {
        let k = json_key_for(&key);
        map.insert(k, py_any_to_json_at(py, &value, depth + 1));
    });

    owned::Value::Object(Box::new(map))
}

#[inline]
#[must_use]
pub fn py_list_to_json(py: Python<'_>, list: &Bound<'_, PyList>) -> Value {
    json_array_at(py, list.iter(), list.len(), 0)
}

fn json_array_at<'py>(
    py: Python<'py>,
    items: impl Iterator<Item = Bound<'py, PyAny>>,
    len: usize,
    depth: usize,
) -> Value {
    let mut vec = Vec::with_capacity(len);
    vec.extend(items.map(|item| py_any_to_json_at(py, &item, depth + 1)));
    Value::Array(Box::new(vec))
}

/// The key text `jsonable_encoder` plus `json.dumps` would write. A key they would reject
/// falls back to its `str()` here, since this conversion cannot fail.
#[inline]
#[must_use]
pub fn json_key_for(key: &Bound<'_, PyAny>) -> String {
    if let Ok(text) = key.cast_exact::<PyString>()
        && let Ok(text) = text.to_str()
    {
        return text.to_owned();
    }
    json_key_text(key.py(), key, 0).unwrap_or_else(|_| {
        key.str()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

/// Converts a Python value into a JSON value. Values nested deeper than the depth limit
/// (a cyclic container, for one) become `null`, since this conversion cannot fail.
#[inline]
#[must_use]
pub fn py_any_to_json(py: Python<'_>, value: &Bound<'_, PyAny>) -> Value {
    py_any_to_json_at(py, value, 0)
}

/// `str(value)` as a JSON string, or `null` when the value has no text.
fn text_or_null(value: &Bound<'_, PyAny>) -> Value {
    value.str().map_or_else(
        |_| simd_json::json!(null),
        |text| simd_json::json!(lossy_text(&text)),
    )
}

fn py_any_to_json_at(py: Python<'_>, value: &Bound<'_, PyAny>, depth: usize) -> Value {
    if depth > MAX_JSON_DEPTH {
        return simd_json::json!(null);
    }
    let next = depth + 1;
    match classify_py_value(py, value) {
        PyJsonKind::None => simd_json::json!(null),
        PyJsonKind::Dict(dict) => py_dict_to_json_at(py, dict, depth),
        PyJsonKind::List(list) => json_array_at(py, list.iter(), list.len(), depth),
        PyJsonKind::Str(s) => simd_json::json!(lossy_text(s)),
        PyJsonKind::Bool(b) => simd_json::json!(b.is_true()),
        PyJsonKind::Int(i) => i
            .extract::<i64>()
            .map_or_else(|_| text_or_null(value), |v| simd_json::json!(v)),

        PyJsonKind::Float(f) => match f.extract::<f64>() {
            Ok(v) if v.is_finite() => simd_json::json!(v),
            _ => simd_json::json!(null),
        },

        PyJsonKind::Tuple(tuple) => json_array_at(py, tuple.iter(), tuple.len(), depth),
        PyJsonKind::Set(set) => json_array_at(py, set.iter(), set.len(), depth),
        PyJsonKind::FrozenSet(fset) => json_array_at(py, fset.iter(), fset.len(), depth),

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
            // JSON mode can reject a field that Python mode still dumps, so an error falls through.
            if let Ok(json) = value.call_method(intern!(py, "model_dump_json"), (), by_alias(py))
                && let Ok(s) = json.cast::<PyString>()
            {
                let mut json_buf = lossy_text(s).into_bytes();
                return simd_json::to_owned_value(&mut json_buf)
                    .unwrap_or_else(|_| simd_json::json!(null));
            }
            value
                .call_method(intern!(py, "model_dump"), (), by_alias(py))
                .map_or_else(
                    |_| simd_json::json!(null),
                    |dumped| py_any_to_json_at(py, &dumped, next),
                )
        }

        PyJsonKind::Dataclass => {
            if let Some(asdict) = dataclasses_asdict(py)
                && let Ok(d) = asdict.call1((value,))
            {
                return py_any_to_json_at(py, &d, next);
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
            text_or_null(value)
        }

        PyJsonKind::Iterable => value.try_iter().map_or_else(
            |_| simd_json::json!(null),
            |iter| {
                Value::Array(Box::new(
                    iter.filter_map(Result::ok)
                        .map(|item| py_any_to_json_at(py, &item, next))
                        .collect(),
                ))
            },
        ),

        PyJsonKind::Object => object_fields(py, value).map_or_else(
            || text_or_null(value),
            |fields| py_any_to_json_at(py, &fields, next),
        ),

        PyJsonKind::Decimal => decimal_number(py, value).map_or_else(
            |_| simd_json::json!(null),
            |number| py_any_to_json_at(py, &number, next),
        ),

        PyJsonKind::UuidOrDecimal
        | PyJsonKind::Path
        | PyJsonKind::IpAddress
        | PyJsonKind::FallbackStr => text_or_null(value),

        PyJsonKind::Enum => {
            if let Ok(inner) = value.getattr(intern!(py, "value")) {
                py_any_to_json_at(py, &inner, next)
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
