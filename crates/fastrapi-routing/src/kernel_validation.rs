//! Rust-native request validation, built on `pydantic-core-kernel` (which is the
//! pure-Rust decoupling of pydantic-core)

//! The kernel keeps pydantic semantics 1:1, so 422 responses
//! are indistinguishable from pydantic's.

use std::sync::{Arc, OnceLock};

use ahash::AHashMap;
use parking_lot::Mutex;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pyo3::prelude::*;

use pydantic_core_kernel::validators::{KValidator, Validator};
use pydantic_core_kernel::{
    CompiledSchema, CoreSchema, DefinitionsBuilder, ErrorType, InputKind, SchemaResult, ValError,
    ValLineError, ValidationError, ValidationState, Value, build_validator,
};
use pyo3::types::{
    PyBool, PyBytes, PyDate, PyDateTime, PyDict, PyFloat, PyInt, PyList, PyString, PyTime, PyTzInfo,
};

use crate::route::PydanticValidator;
use fastrapi_pyutil::json::py_json_response_with_status;
use pydantic_core_kernel::json::{parse_json, serde_to_value};
use pydantic_core_kernel::{ExtraBehavior, Int, LocItem};
use serde_json::{Map, Number, Value as SerdeJsonValue};
use std::env::var;
use std::iter::once;
use std::str::from_utf8;
use std::sync::atomic::{AtomicUsize, Ordering};

const ENV_FLAG: &str = "FASTRAPI_KERNEL_VALIDATION";

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| !matches!(var(ENV_FLAG).as_deref(), Ok("0" | "false" | "False")))
}

pub struct KernelFastPath {
    schema: KernelSchema,
    model_class: Py<PyAny>,
}

enum KernelSchema {
    Raw(CompiledSchema),
    Aliased {
        validator: ParsedJsonValidator,
        core: CoreSchema,
    },
}

struct ParsedJsonValidator {
    validator: Arc<KValidator>,
    title: String,
}

impl ParsedJsonValidator {
    fn compile(schema: &CoreSchema, title: &str) -> SchemaResult<Self> {
        let mut definitions = DefinitionsBuilder::default();
        let validator = build_validator(schema, None, &mut definitions)?;
        definitions.verify_all()?;
        definitions.finish();
        Ok(Self {
            validator,
            title: title.to_owned(),
        })
    }

    fn validate(&self, value: &Value) -> Result<Value, ValidationError> {
        let mut state = ValidationState::new(InputKind::Json);
        self.validator
            .validate(value, &mut state)
            .map_err(|err| self.to_validation_error(err))
    }

    fn to_validation_error(&self, err: ValError) -> ValidationError {
        let line_errors = match err {
            ValError::LineErrors(line_errors) => line_errors,
            ValError::Omit | ValError::UseDefault => vec![ValLineError::new(
                ErrorType::CustomError {
                    error_type: "internal_error".to_owned(),
                    message_template:
                        "Validation unexpectedly returned omit/default at the top level".to_owned(),
                    context: None,
                },
                Value::Null,
            )],
        };
        ValidationError::new(&self.title, line_errors, InputKind::Json)
    }
}

type Cache = Mutex<AHashMap<usize, Option<Arc<KernelFastPath>>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Cache::default)
}

pub(crate) static KERNEL_VALIDATIONS: AtomicUsize = AtomicUsize::new(0);

pub fn validation_count() -> usize {
    KERNEL_VALIDATIONS.load(Ordering::Relaxed)
}

#[pyo3::pyfunction]
#[must_use]
pub fn kernel_validation_count() -> usize {
    validation_count()
}

pub fn maybe_validate_with_kernel(
    py: Python<'_>,
    validator: &PydanticValidator,
    raw_payload: &[u8],
) -> Option<Result<Py<PyAny>, Response>> {
    from_utf8(raw_payload).ok()?;
    let fast = fast_path(py, validator)?;
    KERNEL_VALIDATIONS.fetch_add(1, Ordering::Relaxed);
    validate_body(py, fast, raw_payload)
}

fn fast_path<'v>(py: Python<'_>, validator: &'v PydanticValidator) -> Option<&'v KernelFastPath> {
    if let Some(resolved) = validator.kernel.get() {
        return resolved.as_deref();
    }
    let resolved = resolve_fast_path(py, validator.model_class.bind(py), &validator.name);
    validator.kernel.get_or_init(|| resolved).as_deref()
}

fn resolve_fast_path(
    py: Python<'_>,
    model_class: &Bound<'_, PyAny>,
    name: &str,
) -> Option<Arc<KernelFastPath>> {
    if !enabled() {
        return None;
    }
    let key = model_class.as_ptr() as usize;
    if let Some(cached) = cache().lock().get(&key) {
        return cached.clone();
    }
    let compiled = compile_model(py, model_class, name).map(Arc::new);
    cache().lock().entry(key).or_insert(compiled).clone()
}

fn py_schema_to_json(obj: &Bound<'_, PyAny>) -> Option<serde_json::Value> {
    use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};

    if obj.is_none() {
        return Some(SerdeJsonValue::Null);
    }
    if let Ok(b) = obj.cast::<PyBool>() {
        return Some(b.is_true().into());
    }
    if let Ok(i) = obj.cast::<PyInt>() {
        return i.extract::<i64>().map_or_else(
            |_| {
                i.extract::<u64>()
                    .map(Into::into)
                    .or_else(|_| i.extract::<i128>().map(|v| Number::from_i128(v).into()))
                    .ok()
            },
            |v| Some(v.into()),
        );
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
        return Some(SerdeJsonValue::Array(out));
    }
    if let Ok(tuple) = obj.cast::<PyTuple>() {
        let mut out = Vec::with_capacity(tuple.len());
        for item in tuple.iter() {
            out.push(py_schema_to_json(&item)?);
        }
        return Some(SerdeJsonValue::Array(out));
    }
    if let Ok(dict) = obj.cast::<PyDict>() {
        let mut map = Map::new();
        for (key, value) in dict.iter() {
            let key: String = key.cast::<PyString>().ok()?.to_string();
            map.insert(
                key,
                py_schema_to_json(&value).unwrap_or(SerdeJsonValue::Null),
            );
        }
        return Some(SerdeJsonValue::Object(map));
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

    let extra_allowed = match &schema {
        CoreSchema::Model { config, .. } => {
            config.as_ref().and_then(|c| c.extra_behavior) == Some(ExtraBehavior::Allow)
        }
        _ => false,
    };
    if extra_allowed || schema_has_nested_models(&schema) {
        return None;
    }

    let schema = if schema_has_aliases(&schema) {
        KernelSchema::Aliased {
            validator: ParsedJsonValidator::compile(&schema, name).ok()?,
            core: schema,
        }
    } else {
        KernelSchema::Raw(CompiledSchema::with_title(&schema, None, name).ok()?)
    };
    Some(KernelFastPath {
        schema,
        model_class: model_class.clone().unbind(),
    })
}

#[derive(Clone, Copy)]
struct AliasRules {
    validate_by_alias: bool,
    validate_by_name: bool,
}

impl Default for AliasRules {
    fn default() -> Self {
        Self {
            validate_by_alias: true,
            validate_by_name: false,
        }
    }
}

fn overridden_rules(
    rules: AliasRules,
    config: Option<&pydantic_core_kernel::CoreConfig>,
) -> AliasRules {
    let Some(config) = config else {
        return rules;
    };
    AliasRules {
        validate_by_alias: config.validate_by_alias.unwrap_or(rules.validate_by_alias),
        validate_by_name: config.validate_by_name.unwrap_or(rules.validate_by_name),
    }
}

/// does any field in the schema carry a validation alias?
fn schema_has_aliases(schema: &CoreSchema) -> bool {
    match schema {
        CoreSchema::Model { schema, .. }
        | CoreSchema::ModelField { schema, .. }
        | CoreSchema::Nullable { schema, .. }
        | CoreSchema::Definitions { schema, .. }
        | CoreSchema::Json { schema } => schema_has_aliases(schema),
        CoreSchema::Default { schema, .. } | CoreSchema::WithDefault { schema, .. } => {
            schema.as_ref().is_some_and(|s| schema_has_aliases(s))
        }
        CoreSchema::ModelFields { fields, .. } => fields.iter().any(|(_, field_schema)| {
            matches!(
                field_schema,
                CoreSchema::ModelField {
                    validation_alias: Some(_),
                    ..
                }
            ) || schema_has_aliases(field_schema)
        }),
        CoreSchema::TypedDict { fields, .. } => fields
            .iter()
            .any(|field| field.validation_alias.is_some() || schema_has_aliases(&field.schema)),
        CoreSchema::List { items_schema, .. } | CoreSchema::Set { items_schema, .. } => {
            items_schema.as_ref().is_some_and(|s| schema_has_aliases(s))
        }
        CoreSchema::Dict { values_schema, .. } => values_schema
            .as_ref()
            .is_some_and(|s| schema_has_aliases(s)),
        CoreSchema::Union { choices, .. } => choices.iter().any(schema_has_aliases),
        CoreSchema::TaggedUnion { choices, .. } => {
            choices.iter().any(|(_, s)| schema_has_aliases(s))
        }
        CoreSchema::LaxOrStrict {
            lax_schema,
            strict_schema,
        }
        | CoreSchema::JsonOrPython {
            json: lax_schema,
            python: strict_schema,
        } => schema_has_aliases(lax_schema) || schema_has_aliases(strict_schema),
        _ => false,
    }
}

fn strip_shadowed_name_keys(schema: &CoreSchema, value: &mut Value, rules: AliasRules) {
    match schema {
        CoreSchema::Model { config, schema, .. } => {
            strip_shadowed_name_keys(schema, value, overridden_rules(rules, config.as_ref()));
        }
        CoreSchema::ModelFields { fields, .. } => {
            let specs: Vec<(String, Option<String>, &CoreSchema)> = fields
                .iter()
                .map(|(name, field_schema)| match field_schema {
                    CoreSchema::ModelField {
                        schema,
                        validation_alias,
                    } => (name.clone(), validation_alias.clone(), &**schema),
                    other => (name.clone(), None, other),
                })
                .collect();
            strip_struct_level(value, &specs, rules);
        }
        CoreSchema::TypedDict { config, fields, .. } => {
            let specs: Vec<(String, Option<String>, &CoreSchema)> = fields
                .iter()
                .map(|field| {
                    (
                        field.name.clone(),
                        field.validation_alias.clone(),
                        &*field.schema,
                    )
                })
                .collect();
            strip_struct_level(value, &specs, overridden_rules(rules, config.as_ref()));
        }
        CoreSchema::ModelField { schema, .. }
        | CoreSchema::Nullable { schema, .. }
        | CoreSchema::Json { schema } => strip_shadowed_name_keys(schema, value, rules),
        CoreSchema::Default { schema, .. } | CoreSchema::WithDefault { schema, .. } => {
            if let Some(schema) = schema.as_ref() {
                strip_shadowed_name_keys(schema, value, rules);
            }
        }
        CoreSchema::List { items_schema, .. } | CoreSchema::Set { items_schema, .. } => {
            if let (Some(items_schema), Value::Array(items)) = (items_schema.as_ref(), value) {
                for item in items {
                    strip_shadowed_name_keys(items_schema, item, rules);
                }
            }
        }
        CoreSchema::Dict { values_schema, .. } => {
            if let (Some(values_schema), Value::Object(entries)) = (values_schema.as_ref(), value) {
                for (_, entry) in entries {
                    strip_shadowed_name_keys(values_schema, entry, rules);
                }
            }
        }
        _ => {}
    }
}

fn strip_struct_level(
    value: &mut Value,
    fields: &[(String, Option<String>, &CoreSchema)],
    rules: AliasRules,
) {
    let Value::Object(entries) = value else {
        return;
    };
    let has_key = |entries: &[(String, Value)], key: &str| entries.iter().any(|(k, _)| k == key);

    let shadowed: Vec<&str> = fields
        .iter()
        .filter_map(|(name, alias, _)| {
            let alias = alias.as_deref()?;
            let name_present = has_key(entries, name);
            let alias_present = has_key(entries, alias);
            if name_present
                && ((alias_present && rules.validate_by_alias) || !rules.validate_by_name)
            {
                Some(name.as_str())
            } else {
                None
            }
        })
        .collect();
    if !shadowed.is_empty() {
        entries.retain(|(k, _)| !shadowed.contains(&k.as_str()));
    }

    for (name, alias, inner) in fields {
        let consumed = match alias.as_deref() {
            Some(alias) if rules.validate_by_alias && has_key(entries, alias) => alias,
            Some(_) if rules.validate_by_name && has_key(entries, name) => name.as_str(),
            None if has_key(entries, name) => name.as_str(),
            _ => continue,
        };
        if let Some((_, entry)) = entries.iter_mut().find(|(k, _)| k == consumed) {
            strip_shadowed_name_keys(inner, entry, rules);
        }
    }
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
            | CoreSchema::ModelField { schema, .. }
            | CoreSchema::Definitions { schema, .. } => walk(schema, inside),
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
            // conservative: refs may point at models
            CoreSchema::DefinitionRef { .. } => inside_struct,
            _ => false,
        }
    }
    walk(schema, false)
}
fn validate_body(
    py: Python<'_>,
    fast: &KernelFastPath,
    raw_payload: &[u8],
) -> Option<Result<Py<PyAny>, Response>> {
    let validated = match &fast.schema {
        KernelSchema::Raw(schema) => schema.validate_json(raw_payload),
        KernelSchema::Aliased { validator, core } => {
            let mut parsed = parse_json(raw_payload).ok()?;
            strip_shadowed_name_keys(core, &mut parsed, AliasRules::default());
            validator.validate(&parsed)
        }
    };
    match validated {
        Ok(value) => construct_model(py, fast, &value),
        Err(err) => Some(Err(kernel_error_response(py, &err))),
    }
}

fn construct_model(
    py: Python<'_>,
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

fn value_to_py(py: Python<'_>, value: &Value) -> Option<Py<PyAny>> {
    Some(match value {
        Value::Null => py.None(),
        Value::Bool(b) => PyBool::new(py, *b).to_owned().unbind().into_any(),
        Value::Int(i) => match i {
            Int::I64(v) => PyInt::new(py, *v).unbind().into_any(),
            Int::Big(big) => py
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
        Value::Date(d) => PyDate::new(py, i32::from(d.year), d.month, d.day)
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
                i32::from(dt.date.year),
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
            PyTime::new(py, t.hour, t.minute, t.second, t.microsecond, tz.as_ref())
                .ok()?
                .unbind()
                .into_any()
        }
        Value::Duration(d) => {
            let total_seconds = i64::from(d.day) * 86_400 + i64::from(d.second);
            let signed = if d.positive {
                total_seconds
            } else {
                -total_seconds
            };
            py.import("datetime")
                .ok()?
                .getattr("timedelta")
                .ok()?
                .call1((0i64, signed, i64::from(d.microsecond)))
                .ok()?
                .unbind()
        }
    })
}

fn py_tz(py: Python<'_>, offset_seconds: i32) -> Option<Bound<'_, PyTzInfo>> {
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
                    once(PyString::new(py, "body").unbind().into_any())
                        .chain(detail.loc.iter().map(|loc_item| match loc_item {
                            LocItem::S(s) => PyString::new(py, s).unbind().into_any(),
                            LocItem::I(i) => PyInt::new(py, *i).unbind().into_any(),
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
                let v = value_to_py(py, &serde_to_value(ctx_value));
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
