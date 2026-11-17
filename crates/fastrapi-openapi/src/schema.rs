//! Converts Python types and Pydantic models into JSON schemas.
//! It also makes schema titles and operation IDs in the same format as FastAPI.

use fastrapi_pyutil::json::py_dict_to_json;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use simd_json::json;
use simd_json::owned::Value as JsonValue;
use simd_json::prelude::{
    TypedArrayValue, TypedObjectValue, TypedScalarValue, ValueAsMutArray, ValueAsMutObject,
    ValueObjectAccess,
};

/// Bare builtin return annotations (`-> dict`): FastAPI documents them as the response schema but fastrapi never validates responses against them.
pub const BARE_BUILTIN_RETURNS: [&str; 10] = [
    "dict", "list", "str", "int", "float", "bool", "bytes", "object", "tuple", "set",
];

#[must_use]
pub fn extract_pydantic_schema(py: Python, model: &Bound<PyAny>, mode: &str) -> Option<JsonValue> {
    if let Ok(schema_method) = model.getattr("model_json_schema") {
        let kwargs = PyDict::new(py);
        _ = kwargs.set_item("mode", mode);
        if let Ok(result) = schema_method.call((), Some(&kwargs))
            && let Ok(dict) = result.cast::<PyDict>()
        {
            let mut schema = py_dict_to_json(py, dict);
            strip_null_defaults(&mut schema);
            return Some(schema);
        }
    }

    if let Ok(typing) = py.import("typing")
        && let Ok(origin) = typing.getattr("get_origin").and_then(|f| f.call1((model,)))
        && let Ok(args) = typing.getattr("get_args").and_then(|f| f.call1((model,)))
    {
        let list_type = py.import("builtins").ok()?.getattr("list").ok()?;
        if origin.is(&list_type) {
            let item = args.try_iter().ok()?.next()?.ok()?;
            let schema = extract_pydantic_schema(py, &item, mode).unwrap_or_else(|| json!({}));
            return Some(json!({"type": "array", "items": schema}));
        }
        let union_type = py.import("types").ok()?.getattr("UnionType").ok()?;
        if origin.is(&typing.getattr("Union").ok()?) || origin.is(&union_type) {
            let schemas = args
                .try_iter()
                .ok()?
                .filter_map(Result::ok)
                .filter(|arg| !arg.is_none())
                .filter_map(|arg| extract_pydantic_schema(py, &arg, mode))
                .collect::<Vec<_>>();
            if !schemas.is_empty() {
                return Some(json!({"anyOf": schemas}));
            }
        }
    }

    if let Ok(pydantic) = py.import("pydantic") {
        let adapter_kwargs = PyDict::new(py);
        _ = adapter_kwargs.set_item("_parent_depth", 0);
        if let Ok(adapter) = pydantic
            .getattr("TypeAdapter")
            .and_then(|ty| ty.call((model,), Some(&adapter_kwargs)))
        {
            let kwargs = PyDict::new(py);
            _ = kwargs.set_item("mode", mode);
            if let Ok(result) = adapter.call_method("json_schema", (), Some(&kwargs))
                && let Ok(dict) = result.cast::<PyDict>()
            {
                return Some(py_dict_to_json(py, dict));
            }
        }
    }

    if let Ok(schema_method) = model.getattr("schema")
        && let Ok(result) = schema_method.call0()
        && let Ok(dict) = result.cast::<PyDict>()
    {
        return Some(py_dict_to_json(py, dict));
    }

    None
}

/// Removes all `"default": null` entries from a JSON schema, at all levels.
fn strip_null_defaults(schema: &mut JsonValue) {
    if schema.is_object() {
        let drop_default = schema.get("default").is_some_and(TypedScalarValue::is_null);
        if drop_default && let Some(obj) = schema.as_object_mut() {
            obj.remove(&"default".to_string());
        }
        if let Some(obj) = schema.as_object_mut() {
            for (_, value) in obj.iter_mut() {
                strip_null_defaults(value);
            }
        }
    } else if schema.is_array()
        && let Some(arr) = schema.as_array_mut()
    {
        for value in arr.iter_mut() {
            strip_null_defaults(value);
        }
    }
}

/// Makes a field title in the Pydantic format.
/// For example, `item_id` becomes "Item Id" and `user-agent` becomes "User-Agent".
pub(crate) fn schema_title(name: &str) -> String {
    let name = name.replace('_', " ");
    let mut out = String::with_capacity(name.len());
    let mut capitalize = true;
    for ch in name.chars() {
        if ch.is_alphabetic() {
            if capitalize {
                out.extend(ch.to_uppercase());
            } else {
                out.extend(ch.to_lowercase());
            }
            capitalize = false;
        } else {
            out.push(ch);
            capitalize = true;
        }
    }
    out
}

/// Makes the default operation ID in the FastAPI format.
/// The format is `{name}{path}_{method}`, with each `\W` character changed to `_`.
#[must_use]
pub fn native_operation_id(name: &str, path: &str, method: &str) -> String {
    let mut id = String::with_capacity(name.len() + path.len() + method.len() + 1);
    id.extend(name.chars().chain(path.chars()).map(|c| {
        if c.is_alphanumeric() || c == '_' {
            c
        } else {
            '_'
        }
    }));
    id.push('_');
    id.extend(method.chars().flat_map(char::to_lowercase));
    id
}

/// Converts a Python list of dicts into a list of JSON values.
/// The function ignores items that are not dicts.
pub(crate) fn py_dicts_to_json_vec(py: Python<'_>, value: &Py<PyAny>) -> Option<Vec<JsonValue>> {
    let list = value.extract::<Vec<Py<PyAny>>>(py).ok()?;
    Some(
        list.into_iter()
            .filter_map(|item| {
                item.bind(py)
                    .cast::<PyDict>()
                    .ok()
                    .map(|d| py_dict_to_json(py, d))
            })
            .collect(),
    )
}
