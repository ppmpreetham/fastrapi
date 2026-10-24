//! Converts Python types and Pydantic models into JSON schemas.
//! It also makes schema titles and operation IDs in the same format as FastAPI.

use fastrapi_core::params::ParameterConstraints;
use fastrapi_pyutil::imports::{ENUM, TYPING};
use fastrapi_pyutil::json::{py_any_to_json, py_dict_to_json};
use fastrapi_routing::pydantic;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString, PyType};
use simd_json::json;
use simd_json::owned::Value as JsonValue;
use simd_json::prelude::{
    TypedArrayValue, TypedObjectValue, TypedScalarValue, ValueAsMutArray, ValueAsMutObject,
    ValueObjectAccess,
};
use std::collections::HashMap;

fastrapi_pyutil::cached_py_import!(TYPING_GET_ORIGIN, "typing", "get_origin");

#[must_use]
pub fn extract_pydantic_schema(py: Python, model: &Bound<PyAny>, mode: &str) -> Option<JsonValue> {
    // Pydantic v2
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

    // Pydantic v1
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

pub(crate) fn get_schema_name(model: &Bound<PyAny>) -> String {
    model
        .getattr("__name__")
        .ok()
        .and_then(|name| name.extract::<String>().ok())
        .unwrap_or_else(|| {
            model.str().map_or_else(
                |_| "UnknownSchema".to_string(),
                |value| {
                    value
                        .to_string_lossy()
                        .replace(['[', ']', ' ', '\'', ','], "_")
                },
            )
        })
}

/// Tells if the type hint is a `typing.Literal`.
/// The check is `typing.get_origin(x) is typing.Literal`.
fn is_literal(py: Python<'_>, type_hint: &Bound<PyAny>) -> bool {
    TYPING_GET_ORIGIN
        .get(py)
        .ok()
        .and_then(|get_origin| get_origin.call1((type_hint,)).ok())
        .and_then(|origin| {
            TYPING
                .get(py)
                .ok()?
                .getattr(intern!(py, "Literal"))
                .ok()
                .map(|literal| origin.is(&literal))
        })
        .unwrap_or(false)
}

/// Tells if the type hint is a subclass of `enum.Enum`.
fn is_enum_class(py: Python<'_>, type_hint: &Bound<PyAny>) -> bool {
    type_hint
        .cast::<PyType>()
        .ok()
        .and_then(|ty| {
            ENUM.get(py)
                .ok()?
                .getattr(intern!(py, "Enum"))
                .ok()
                .and_then(|base| ty.is_subclass(base.as_any()).ok())
        })
        .unwrap_or(false)
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

/// Makes the title of the combined body schema in the FastAPI format.
/// The title is the operation ID with the prefix `Body_`.
pub(crate) fn body_model_name(
    handler_func: &Bound<'_, pyo3::PyAny>,
    path: &str,
    method: &str,
) -> String {
    let func_name = handler_func
        .getattr("__name__")
        .ok()
        .and_then(|name| name.extract::<String>().ok())
        .unwrap_or_else(|| "unknown".to_string());
    format!("Body_{}", native_operation_id(&func_name, path, method))
}

/// Makes the default operation ID in the FastAPI format.
/// The format is `{name}{path}_{method}`, with each `\W` character changed to `_`.
pub(crate) fn native_operation_id(name: &str, path: &str, method: &str) -> String {
    let joined = format!("{name}{path}");
    let sanitized: String = joined
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{sanitized}_{}", method.to_lowercase())
}

pub(crate) fn python_type_to_openapi_type(
    py: Python<'_>,
    type_hint: &Bound<PyAny>,
    schemas: &mut HashMap<String, JsonValue>,
) -> JsonValue {
    if pydantic::is_pydantic_model(py, type_hint) {
        let schema_name = get_schema_name(type_hint);
        return json!({
            "$ref": format!("#/components/schemas/{schema_name}")
        });
    }

    // `Optional[X]` / `Union[A, B]` → anyOf, the way pydantic renders them
    if let Some(args) = fastrapi_routing::annotations::union_args(py, type_hint) {
        let has_none = args
            .iter()
            .any(|arg| fastrapi_routing::annotations::is_none_type(&arg));
        let mut any_of: Vec<JsonValue> = args
            .iter()
            .filter(|arg| !fastrapi_routing::annotations::is_none_type(arg))
            .map(|arg| python_type_to_openapi_type(py, &arg, schemas))
            .collect();
        if !any_of.is_empty() {
            if has_none {
                any_of.push(json!({ "type": "null" }));
            }
            return json!({ "anyOf": any_of });
        }
    }

    // `Literal["x", "y"]` → enum schema
    if is_literal(py, type_hint) {
        let values: Vec<JsonValue> = type_hint
            .getattr("__args__")
            .ok()
            .and_then(|args| args.try_iter().ok())
            .map(|iter| {
                iter.filter_map(Result::ok)
                    .map(|a| py_any_to_json(py, &a))
                    .collect()
            })
            .unwrap_or_default();
        if !values.is_empty() {
            let all_str = values.iter().all(TypedScalarValue::is_str);
            let all_int = values.iter().all(TypedScalarValue::is_i64);
            let mut schema = json!({ "enum": values });
            if let Some(obj) = schema.as_object_mut() {
                if all_str {
                    obj.insert("type".to_string(), json!("string"));
                } else if all_int {
                    obj.insert("type".to_string(), json!("integer"));
                }
            }
            return schema;
        }
    }

    // enum classes get a components schema and a `$ref`, like pydantic models
    if is_enum_class(py, type_hint) {
        let schema_name = get_schema_name(type_hint);
        schemas.entry(schema_name.clone()).or_insert_with(|| {
            let values: Vec<JsonValue> = type_hint
                .try_iter()
                .ok()
                .map(|iter| {
                    iter.filter_map(|member| {
                        let member = member.ok()?;
                        let value = member.getattr(intern!(py, "value")).ok()?;
                        Some(py_any_to_json(py, &value))
                    })
                    .collect()
                })
                .unwrap_or_default();
            let all_str = values.iter().all(TypedScalarValue::is_str);
            let all_int = values.iter().all(TypedScalarValue::is_i64);
            let mut schema = json!({ "enum": values, "title": schema_name });
            if let Some(obj) = schema.as_object_mut() {
                if all_str {
                    obj.insert("type".to_string(), json!("string"));
                } else if all_int {
                    obj.insert("type".to_string(), json!("integer"));
                }
            }
            schema
        });
        return json!({ "$ref": format!("#/components/schemas/{schema_name}") });
    }

    if let Ok(name_attr) = type_hint.getattr("__name__")
        && let Ok(py_str) = name_attr.cast::<PyString>()
        && let Ok(name_str) = py_str.to_str()
    {
        match name_str {
            "str" => return json!({ "type": "string" }),
            "int" => return json!({ "type": "integer" }),
            "float" => return json!({ "type": "number" }),
            "bool" => return json!({ "type": "boolean" }),
            "list" | "set" | "tuple" => {
                let items = type_hint
                    .getattr("__args__")
                    .ok()
                    .and_then(|args| args.get_item(0).ok())
                    .map_or_else(
                        || json!({ "type": "string" }),
                        |elem| python_type_to_openapi_type(py, &elem, schemas),
                    );
                return json!({ "type": "array", "items": items });
            }
            "dict" => return json!({ "type": "object" }),
            _ => {}
        }
    }

    if let Ok(type_repr) = type_hint.str()
        && let Ok(type_name) = type_repr.to_str()
    {
        if type_name.contains("List") || type_name.contains("list") {
            let items = type_hint
                .getattr("__args__")
                .ok()
                .and_then(|args| args.get_item(0).ok())
                .map_or_else(
                    || json!({ "type": "string" }),
                    |elem| python_type_to_openapi_type(py, &elem, schemas),
                );
            return json!({ "type": "array", "items": items });
        }

        if type_name.contains("Dict") || type_name.contains("dict") {
            return json!({
                "type": "object"
            });
        }
    }

    json!({ "type": "string" })
}

pub(crate) fn apply_parameter_constraints(
    mut schema: JsonValue,
    constraints: &ParameterConstraints,
) -> JsonValue {
    if let Some(object) = schema.as_object_mut() {
        if let Some(gt) = constraints.gt {
            object.insert("exclusiveMinimum".to_string(), json!(gt));
        }
        if let Some(ge) = constraints.ge {
            object.insert("minimum".to_string(), json!(ge));
        }
        if let Some(lt) = constraints.lt {
            object.insert("exclusiveMaximum".to_string(), json!(lt));
        }
        if let Some(le) = constraints.le {
            object.insert("maximum".to_string(), json!(le));
        }
        if let Some(min_length) = constraints.min_length {
            object.insert("minLength".to_string(), json!(min_length));
        }
        if let Some(max_length) = constraints.max_length {
            object.insert("maxLength".to_string(), json!(max_length));
        }

        if let Some(pattern) = &constraints.pattern {
            object.insert("pattern".to_string(), json!(pattern.as_str()));
        }
    }
    schema
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
