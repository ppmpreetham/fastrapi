use axum::{
    body::Body,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use bytes::{BufMut, BytesMut};
use pyo3::types::{
    PyAny, PyBool, PyByteArray, PyBytes, PyDict, PyFloat, PyFrozenSet, PyInt, PyList, PySet,
    PyTuple,
};
use pyo3::{prelude::*, types::PyString};
use serde_json::{Map, Value};
use serde_pyobject::to_pyobject;
use std::cell::RefCell;

thread_local! {
    /// Per-thread JSON response buffer. `serde_json::to_writer` writes here,
    /// then we `split()` off the written portion as immutable `Bytes` (zero
    /// copy) and feed it directly into the response body. The cell keeps the
    /// underlying allocation across requests so steady-state response building
    /// pays no buffer alloc.
    static RESPONSE_BUF: RefCell<BytesMut> = RefCell::new(BytesMut::with_capacity(1024));
}

/// Serialize a `serde_json::Value` into an axum JSON response, reusing a
/// per-thread `BytesMut` buffer. Replaces `Json(value).into_response()`.
#[inline]
pub fn json_response(py: Python<'_>, value: &Value) -> Response {
    json_response_with_status(py, StatusCode::OK, value)
}

#[inline]
pub fn json_response_with_status(py: Python<'_>, status: StatusCode, value: &Value) -> Response {
    let bytes = RESPONSE_BUF.with(|cell| {
        let mut buf = cell.take();
        buf.clear();
        // BytesMut implements bytes::BufMut; serde_json::to_writer can write
        // into anything io::Write. BytesMut::writer() bridges the two.
        py.detach(|| {
            let _ = serde_json::to_writer((&mut buf).writer(), value);
        });
        let bytes = buf.split().freeze();
        cell.replace(buf);
        bytes
    });

    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(bytes))
        .unwrap()
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
    match to_pyobject(py, value) {
        Ok(obj) => obj.into(),
        Err(e) => {
            eprintln!("Error converting JSON to Python object: {}", e);
            format!("Error: {}", e).into_pyobject(py).unwrap().into()
        }
    }
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

    if let Ok(dict) = obj.cast::<PyDict>() {
        return json_response_with_status(py, status, &py_dict_to_json(py, dict));
    }

    if let Ok(list) = obj.cast::<PyList>() {
        return json_response_with_status(py, status, &py_list_to_json(py, list));
    }

    if let Ok(b) = obj.cast::<PyBool>() {
        return json_response_with_status(py, status, &Value::Bool(b.is_true()));
    }

    if let Ok(s) = obj.cast::<PyString>() {
        if let Ok(slice) = s.to_str() {
            return json_response_with_status(
                py,
                status,
                &Value::String(std::borrow::Cow::Borrowed(slice).into_owned()),
            );
        }
    }

    if let Ok(i) = obj.cast::<PyInt>() {
        if let Ok(v) = i.extract::<i64>() {
            return json_response_with_status(py, status, &Value::Number(v.into()));
        }
    }

    if let Ok(f) = obj.cast::<PyFloat>() {
        let v = f.value(); // Native PyFloat zero-overhead extractor macro
        if let Some(n) = serde_json::Number::from_f64(v) {
            return json_response_with_status(py, status, &Value::Number(n));
        }
        return json_response_with_status(py, status, &Value::Null);
    }

    // 7. General structural fallback serializer
    json_response_with_status(py, status, &py_any_to_json(py, obj))
}
#[inline]
pub fn py_dict_to_json(py: Python<'_>, dict: &Bound<'_, PyDict>) -> Value {
    let mut map = Map::with_capacity(dict.len());

    dict.iter().for_each(|(key, value)| {
        let k = json_key_for(&key);
        map.insert(k, py_any_to_json(py, &value));
    });

    Value::Object(map)
}

#[inline]
pub fn py_list_to_json(py: Python<'_>, list: &Bound<'_, PyList>) -> Value {
    let mut vec = Vec::with_capacity(list.len());

    vec.extend(list.iter().map(|item| py_any_to_json(py, &item)));

    Value::Array(vec)
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

/// Walk an arbitrary Python value into a serde_json::Value.
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
        return Value::Null;
    }

    if let Ok(dict) = value.cast::<PyDict>() {
        return py_dict_to_json(py, dict);
    }
    if let Ok(list) = value.cast::<PyList>() {
        return py_list_to_json(py, list);
    }
    if let Ok(s) = value.cast::<PyString>() {
        return Value::String(s.to_str().unwrap_or_default().to_owned());
    }
    if let Ok(b) = value.cast::<PyBool>() {
        return Value::Bool(b.is_true());
    }
    if let Ok(i) = value.cast::<PyInt>() {
        if let Ok(v) = i.extract::<i64>() {
            return Value::Number(v.into());
        }
        // bigints that don't fit in i64: fall back to string to preserve precision.
        if let Ok(s) = value.str() {
            if let Ok(s) = s.to_str() {
                return Value::String(s.to_owned());
            }
        }
    }
    if let Ok(f) = value.cast::<PyFloat>() {
        if let Ok(v) = f.extract::<f64>() {
            return serde_json::Number::from_f64(v)
                .map(Value::Number)
                .unwrap_or(Value::Null);
        }
    }

    if let Ok(tuple) = value.cast::<PyTuple>() {
        let mut vec = Vec::with_capacity(tuple.len());
        vec.extend(tuple.iter().map(|item| py_any_to_json(py, &item)));

        return Value::Array(vec);
    }
    if let Ok(set) = value.cast::<PySet>() {
        let mut vec = Vec::with_capacity(set.len());
        vec.extend(set.iter().map(|item| py_any_to_json(py, &item)));

        return Value::Array(vec);
    }
    if let Ok(fset) = value.cast::<PyFrozenSet>() {
        let mut vec = Vec::with_capacity(fset.len());
        vec.extend(fset.iter().map(|item| py_any_to_json(py, &item)));

        return Value::Array(vec);
    }

    if let Ok(b) = value.cast::<PyBytes>() {
        return bytes_to_json(b.as_bytes());
    }
    if let Ok(b) = value.cast::<PyByteArray>() {
        let snapshot: Vec<u8> = unsafe { b.as_bytes().to_vec() };
        return bytes_to_json(&snapshot);
    }

    // Duck-typed conversions. Order matters: pydantic BaseModel exposes
    // `model_dump`; dataclasses expose `__dataclass_fields__`; Enum exposes
    // `value`; datetime/date/time/UUID/Decimal are matched by class name to
    // avoid importing each module on the hot path.
    if let Ok(true) = value.hasattr("model_dump") {
        if let Ok(dumped) = value.call_method0("model_dump") {
            return py_any_to_json(py, &dumped);
        }
    }
    if let Ok(true) = value.hasattr("__dataclass_fields__") {
        if let Ok(asdict) = py
            .import("dataclasses")
            .and_then(|m| m.getattr("asdict"))
            .and_then(|f| f.call1((value,)))
        {
            return py_any_to_json(py, &asdict);
        }
    }
    if let Ok(true) = value.hasattr("isoformat") {
        if let Ok(s) = value.call_method0("isoformat") {
            if let Ok(s) = s.cast_into::<PyString>() {
                return Value::String(s.to_str().unwrap_or_default().to_owned());
            }
        }
    }
    if let Ok(class_name) = value
        .get_type()
        .name()
        .map(|n| n.to_str().unwrap_or_default().to_owned())
    {
        match class_name.as_str() {
            "UUID" => {
                if let Ok(s) = value.str() {
                    if let Ok(s) = s.to_str() {
                        return Value::String(s.to_owned());
                    }
                }
            }
            "Decimal" => {
                if let Ok(s) = value.str() {
                    if let Ok(s) = s.to_str() {
                        return Value::String(s.to_owned());
                    }
                }
            }
            _ => {}
        }
    }
    if let Ok(true) = value.hasattr("value") {
        let is_enum = value
            .get_type()
            .getattr("__bases__")
            .ok()
            .and_then(|b| b.cast_into::<PyTuple>().ok())
            .map(|t| {
                t.iter().any(|base| {
                    base.getattr("__name__")
                        .ok()
                        .and_then(|n| n.cast_into::<PyString>().ok())
                        .and_then(|s| s.to_str().ok().map(|s| s.to_owned()))
                        .map(|name| {
                            matches!(
                                name.as_str(),
                                "Enum" | "IntEnum" | "StrEnum" | "Flag" | "IntFlag"
                            )
                        })
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        if is_enum {
            if let Ok(inner) = value.getattr("value") {
                return py_any_to_json(py, &inner);
            }
        }
    }

    if let Ok(s) = value.str() {
        if let Ok(s) = s.to_str() {
            return Value::String(s.to_owned());
        }
    }
    Value::Null
}

#[inline]
fn bytes_to_json(b: &[u8]) -> Value {
    match std::str::from_utf8(b) {
        Ok(s) => Value::String(s.to_owned()),
        Err(_) => Value::Array(b.iter().map(|&x| Value::Number(x.into())).collect()),
    }
}
