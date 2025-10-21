use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};
use serde_json::{json, Map, Value};
use serde_pyobject::to_pyobject;

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

/// Optimized response conversion with early returns
#[inline]
pub fn py_to_response(py: Python<'_>, obj: &Bound<'_, PyAny>) -> Response {
    // Check None first (common case)
    if obj.is_none() {
        return StatusCode::NO_CONTENT.into_response();
    }

    // Try simple scalar types first (most common, ordered by frequency)
    if let Ok(s) = obj.extract::<String>() {
        return s.into_response();
    }
    if let Ok(i) = obj.extract::<i64>() {
        return Json(json!(i)).into_response();
    }
    if let Ok(b) = obj.extract::<bool>() {
        return Json(json!(b)).into_response();
    }
    if let Ok(f) = obj.extract::<f64>() {
        return Json(json!(f)).into_response();
    }

    // Try complex types (use cast for performance)
    if let Ok(dict) = obj.cast::<PyDict>() {
        return Json(py_dict_to_json(py, dict)).into_response();
    }
    if let Ok(list) = obj.cast::<PyList>() {
        return Json(py_list_to_json(py, list)).into_response();
    }

    // Fallback
    Json(json!(format!("{:?}", obj))).into_response()
}

/// Optimized dict to JSON conversion with capacity hint
#[inline]
pub fn py_dict_to_json(py: Python<'_>, dict: &Bound<'_, PyDict>) -> Value {
    let mut map = Map::with_capacity(dict.len());

    for (key, value) in dict.iter() {
        if let Ok(k) = key.extract::<String>() {
            map.insert(k, py_any_to_json(py, &value));
        }
    }

    Value::Object(map)
}

/// Optimized list to JSON conversion with capacity hint
#[inline]
pub fn py_list_to_json(py: Python<'_>, list: &Bound<'_, PyList>) -> Value {
    let mut vec = Vec::with_capacity(list.len());

    for item in list.iter() {
        vec.push(py_any_to_json(py, &item));
    }

    Value::Array(vec)
}

/// Fast any to JSON conversion with early returns
#[inline]
fn py_any_to_json(py: Python<'_>, value: &Bound<'_, PyAny>) -> Value {
    if value.is_none() {
        return Value::Null;
    }

    // Try scalar types first (ordered by frequency)
    if let Ok(b) = value.extract::<bool>() {
        return Value::Bool(b);
    }
    if let Ok(i) = value.extract::<i64>() {
        return Value::Number(i.into());
    }
    if let Ok(f) = value.extract::<f64>() {
        return serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null);
    }
    if let Ok(s) = value.extract::<String>() {
        return Value::String(s);
    }

    // Try complex types
    if let Ok(dict) = value.cast::<PyDict>() {
        return py_dict_to_json(py, dict);
    }
    if let Ok(list) = value.cast::<PyList>() {
        return py_list_to_json(py, list);
    }

    // Fallback
    Value::String(format!("{:?}", value))
}
