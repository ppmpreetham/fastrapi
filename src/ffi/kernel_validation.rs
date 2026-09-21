//! Rust-native request validation, built on `pydantic-core-kernel` (which is the
//! pure-Rust decoupling of pydantic-core)

//! The kernel keeps pydantic semantics 1:1, so 422 responses are indistinguishable from pydantic's.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pyo3::prelude::*;

use pydantic_core_kernel::{CompiledSchema, CoreSchema, ValidationError, Value};
use pyo3::types::{
    PyBool, PyBytes, PyDate, PyDateTime, PyDict, PyFloat, PyInt, PyList, PyString, PyTime, PyTzInfo,
};

use crate::types::route::PydanticValidator;
use crate::utils::json::py_json_response_with_status;

const ENV_FLAG: &str = "FASTRAPI_KERNEL_VALIDATION";

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        !matches!(
            std::env::var(ENV_FLAG).as_deref(),
            Ok("0") | Ok("false") | Ok("False")
        )
    })
}

pub struct KernelFastPath {
    schema: CompiledSchema,
    model_class: Py<PyAny>,
    construct_safe: bool,
}

type Cache = Mutex<HashMap<usize, Option<Arc<KernelFastPath>>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) static KERNEL_VALIDATIONS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

pub fn validation_count() -> usize {
    KERNEL_VALIDATIONS.load(std::sync::atomic::Ordering::Relaxed)
}

#[pyo3::pyfunction]
pub fn kernel_validation_count() -> usize {
    validation_count()
}

pub fn maybe_validate_with_kernel<'py>(
    py: Python<'py>,
    validator: &PydanticValidator,
    raw_payload: &[u8],
) -> Option<Result<Py<PyAny>, Response>> {
    std::str::from_utf8(raw_payload).ok()?;
    let fast = fast_path_for(py, validator.model_class.bind(py), &validator.name)?;
    KERNEL_VALIDATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    validate_body(py, &fast, raw_payload)
}

fn fast_path_for<'py>(
    py: Python<'py>,
    model_class: &Bound<'py, PyAny>,
    name: &str,
) -> Option<Arc<KernelFastPath>> {
    if !enabled() {
        return None;
    }
    let key = model_class.as_ptr() as usize;

    if let Some(cached) = cache().lock().expect("kernel cache").get(&key) {
        return cached.clone();
    }

    let compiled = compile_model(py, model_class, name).map(Arc::new);
    cache()
        .lock()
        .expect("kernel cache")
        .insert(key, compiled.clone());
    compiled
}

fn py_schema_to_json(obj: &Bound<'_, PyAny>) -> Option<serde_json::Value> {
    use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};

    if obj.is_none() {
        return Some(serde_json::Value::Null);
    }
    if let Ok(b) = obj.cast::<PyBool>() {
        return Some(b.is_true().into());
    }
    if let Ok(i) = obj.cast::<PyInt>() {
        return match i.extract::<i64>() {
            Ok(v) => Some(v.into()),
            Err(_) => i
                .extract::<u64>()
                .map(|v| v.into())
                .or_else(|_| {
                    i.extract::<i128>()
                        .map(|v| serde_json::Number::from_i128(v).into())
                })
                .ok(),
        };
    }
    if let Ok(f) = obj.cast::<PyFloat>() {
        return Some(f.value().into());
    }
    if let Ok(st) = obj.cast::<PyString>() {
        return Some(st.to_cow().ok()?.to_string().into());
    }
    if let Ok(list) = obj.cast::<PyList>() {
        let mut out = Vec::with_capacity(list.len());
        for item in list.iter() {
            out.push(py_schema_to_json(&item)?);
        }
        return Some(serde_json::Value::Array(out));
    }
    if let Ok(tuple) = obj.cast::<PyTuple>() {
        let mut out = Vec::with_capacity(tuple.len());
        for item in tuple.iter() {
            out.push(py_schema_to_json(&item)?);
        }
        return Some(serde_json::Value::Array(out));
    }
    if let Ok(dict) = obj.cast::<PyDict>() {
        let mut map = serde_json::Map::new();
        for (key, value) in dict.iter() {
            let key: String = key.cast::<PyString>().ok()?.to_string();
            map.insert(
                key,
                py_schema_to_json(&value).unwrap_or(serde_json::Value::Null),
            );
        }
        return Some(serde_json::Value::Object(map));
    }
    None
}

fn compile_model<'py>(
    _py: Python<'py>,
    model_class: &Bound<'py, PyAny>,
    name: &str,
) -> Option<KernelFastPath> {
    let core_schema = model_class.getattr("__pydantic_core_schema__").ok()?;
    let json_value = py_schema_to_json(&core_schema)?;
    let schema: CoreSchema = serde_json::from_value(json_value).ok()?;

    let compiled = CompiledSchema::with_title(&schema, None, name).ok()?;
    let extra_allowed = match &schema {
        CoreSchema::Model { config, .. } => {
            config.as_ref().and_then(|c| c.extra_behavior)
                == Some(pydantic_core_kernel::ExtraBehavior::Allow)
        }
        _ => false,
    };

    Some(KernelFastPath {
        schema: compiled,
        model_class: model_class.clone().unbind(),
        construct_safe: !schema_has_nested_models(&schema) && !extra_allowed,
    })
}

fn schema_has_nested_models(schema: &CoreSchema) -> bool {
    fn walk(schema: &CoreSchema, inside_struct: bool) -> bool {
        let is_struct = matches!(
            schema,
            CoreSchema::Model { .. } | CoreSchema::TypedDict { .. }
        );
        if is_struct && inside_struct {
            return true;
        }
        let inside = inside_struct || is_struct;
        match schema {
            CoreSchema::List { items_schema, .. } | CoreSchema::Set { items_schema, .. } => {
                items_schema.as_ref().is_some_and(|s| walk(s, inside))
            }
            CoreSchema::Dict {
                keys_schema,
                values_schema,
                ..
            } => {
                keys_schema.as_ref().is_some_and(|s| walk(s, inside))
                    || values_schema.as_ref().is_some_and(|s| walk(s, inside))
            }
            CoreSchema::Nullable { schema, .. }
            | CoreSchema::Json { schema }
            | CoreSchema::Model { schema, .. }
            | CoreSchema::ModelField { schema, .. } => walk(schema, inside),
            CoreSchema::WithDefault { schema, .. } | CoreSchema::Default { schema, .. } => {
                schema.as_ref().is_some_and(|s| walk(s, inside))
            }
            CoreSchema::Union { choices, .. } => choices.iter().any(|s| walk(s, inside)),
            CoreSchema::TaggedUnion { choices, .. } => choices.iter().any(|(_, s)| walk(s, inside)),
            CoreSchema::JsonOrPython { json, python } => walk(json, inside) || walk(python, inside),
            CoreSchema::LaxOrStrict {
                lax_schema,
                strict_schema,
            } => walk(lax_schema, inside) || walk(strict_schema, inside),
            CoreSchema::Definitions { schema, .. } => walk(schema, inside),
            // conservative: refs may point at models
            CoreSchema::DefinitionRef { .. } => inside_struct,
            _ => false,
        }
    }
    walk(schema, false)
}

fn validate_body<'py>(
    py: Python<'py>,
    fast: &KernelFastPath,
    raw_payload: &[u8],
) -> Option<Result<Py<PyAny>, Response>> {
    match fast.schema.validate_json(raw_payload) {
        Ok(value) => {
            if !fast.construct_safe {
                return None;
            }
            construct_model(py, fast, &value)
        }
        Err(err) => Some(Err(kernel_error_response(py, &err))),
    }
}

fn construct_model<'py>(
    py: Python<'py>,
    fast: &KernelFastPath,
    value: &Value,
) -> Option<Result<Py<PyAny>, Response>> {
    let Value::Object(entries) = value else {
        return None;
    };
    let kwargs = PyDict::new(py);
    for (key, field_value) in entries {
        let py_value = value_to_py(py, field_value)?;
        kwargs.set_item(key, py_value).ok()?;
    }
    let cls = fast.model_class.bind(py);
    let construct = cls.getattr("model_construct").ok()?;
    match construct.call((), Some(&kwargs)) {
        Ok(instance) => Some(Ok(instance.unbind())),
        Err(_) => None,
    }
}

fn value_to_py<'py>(py: Python<'py>, value: &Value) -> Option<Py<PyAny>> {
    Some(match value {
        Value::Null => py.None(),
        Value::Bool(b) => PyBool::new(py, *b).to_owned().unbind().into_any(),
        Value::Int(i) => match i {
            pydantic_core_kernel::Int::I64(v) => PyInt::new(py, *v).unbind().into_any(),
            pydantic_core_kernel::Int::Big(big) => py
                .import("builtins")
                .ok()?
                .call_method1("int", (big.to_string(),))
                .ok()?
                .unbind(),
        },
        Value::Float(f) => PyFloat::new(py, *f).unbind().into_any(),
        Value::Str(s) => PyString::new(py, s).unbind().into_any(),
        Value::Bytes(b) => PyBytes::new(py, b).unbind().into_any(),
        Value::Array(items) => {
            let mut converted = Vec::with_capacity(items.len());
            for item in items {
                converted.push(value_to_py(py, item)?);
            }
            PyList::new(py, converted).ok()?.unbind().into_any()
        }
        Value::Object(entries) => {
            let dict = PyDict::new(py);
            for (key, item) in entries {
                dict.set_item(key, value_to_py(py, item)?).ok()?;
            }
            dict.unbind().into_any()
        }
        Value::Date(d) => PyDate::new(py, d.year as i32, d.month, d.day)
            .ok()?
            .unbind()
            .into_any(),
        Value::DateTime(dt) => {
            let tz = match dt.time.tz_offset {
                None => None,
                Some(offset) => Some(py_tz(py, offset)?),
            };
            PyDateTime::new(
                py,
                dt.date.year as i32,
                dt.date.month,
                dt.date.day,
                dt.time.hour,
                dt.time.minute,
                dt.time.second,
                dt.time.microsecond,
                tz.as_ref(),
            )
            .ok()?
            .unbind()
            .into_any()
        }
        Value::Time(t) => {
            let tz = match t.tz_offset {
                None => None,
                Some(offset) => Some(py_tz(py, offset)?),
            };
            PyTime::new(
                py,
                t.hour,
                t.minute,
                t.second,
                t.microsecond,
                tz.as_ref(),
            )
            .ok()?
            .unbind()
            .into_any()
        }
        Value::Duration(d) => {
            let total_seconds = d.day as i64 * 86_400 + d.second as i64;
            let signed = if d.positive {
                total_seconds
            } else {
                -total_seconds
            };
            py.import("datetime")
                .ok()?
                .getattr("timedelta")
                .ok()?
                .call1((0i64, signed, d.microsecond as i64))
                .ok()?
                .unbind()
        }
    })
}

fn py_tz<'py>(py: Python<'py>, offset_seconds: i32) -> Option<Bound<'py, PyTzInfo>> {
    let datetime = py.import("datetime").ok()?;
    let tz = if offset_seconds == 0 {
        datetime.getattr("timezone").ok()?.getattr("utc").ok()?
    } else {
        let timedelta = datetime
            .getattr("timedelta")
            .ok()?
            .call1((offset_seconds,))
            .ok()?;
        datetime
            .getattr("timezone")
            .ok()?
            .call1((timedelta,))
            .ok()?
    };
    tz.cast_into::<PyTzInfo>().ok()
}

fn kernel_error_response(py: Python<'_>, err: &ValidationError) -> Response {
    use pyo3::types::{PyDict, PyInt, PyList, PyString};

    let detail_list = PyList::empty(py);
    for detail in err.errors() {
        let item = PyDict::new(py);
        if item.set_item("type", detail.r#type.as_str()).is_err()
            || item
                .set_item(
                    // fastapi roots body errors at `["body", ...]`
                    "loc",
                    std::iter::once(PyString::new(py, "body").unbind().into_any())
                        .chain(detail.loc.iter().map(|loc_item| match loc_item {
                            pydantic_core_kernel::LocItem::S(s) => {
                                PyString::new(py, s).unbind().into_any()
                            }
                            pydantic_core_kernel::LocItem::I(i) => {
                                PyInt::new(py, *i).unbind().into_any()
                            }
                        }))
                        .collect::<Vec<_>>(),
                )
                .is_err()
            || item.set_item("msg", detail.msg.as_str()).is_err()
        {
            return fallback_response();
        }
        if let Some(input) = &detail.input {
            match value_to_py(py, input) {
                Some(v) => {
                    if item.set_item("input", v).is_err() {
                        return fallback_response();
                    }
                }
                None => return fallback_response(),
            }
        }
        if let Some(ctx) = &detail.ctx {
            let ctx_dict = PyDict::new(py);
            for (key, ctx_value) in ctx {
                let v = value_to_py(py, &pydantic_core_kernel::json::serde_to_value(ctx_value));
                match v {
                    Some(v) => {
                        if ctx_dict.set_item(key, v).is_err() {
                            return fallback_response();
                        }
                    }
                    None => return fallback_response(),
                }
            }
            if item.set_item("ctx", ctx_dict).is_err() {
                return fallback_response();
            }
        }
        // fastapi drops pydantic's `url` field from the wire format
        if detail_list.append(item).is_err() {
            return fallback_response();
        }
    }

    let dict = PyDict::new(py);
    if dict.set_item("detail", detail_list).is_err() {
        return fallback_response();
    }
    match py_json_response_with_status(py, StatusCode::UNPROCESSABLE_ENTITY, dict.as_any()) {
        Ok(resp) => resp,
        Err(_) => fallback_response(),
    }
}

fn fallback_response() -> Response {
    (StatusCode::UNPROCESSABLE_ENTITY, "Validation failed").into_response()
}
