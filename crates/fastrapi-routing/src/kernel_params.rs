use crate::kernel_validation::value_to_py;
use pydantic_core_kernel::json::serde_to_value;
use pydantic_core_kernel::{CompiledSchema, CoreSchema, Int, Value};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBool, PyDict, PyFloat, PyInt, PyList, PySet, PyString, PyTuple};
use serde_json::{Map, Number, Value as Json};
use simd_json::{OwnedValue, StaticNode};

const IGNORED: [&str; 3] = ["metadata", "ref", "serialization"];

fastrapi_pyutil::cached_py_import!(UUID, "uuid", "UUID");
fastrapi_pyutil::cached_py_import!(SAFE_UNKNOWN, "uuid", "SafeUUID", "unknown");
fastrapi_pyutil::cached_py_import!(OBJECT_NEW, "builtins", "object", "__new__");
fastrapi_pyutil::cached_py_import!(OBJECT_SETATTR, "builtins", "object", "__setattr__");

#[derive(Debug)]
enum Shape {
    Plain,
    Datetime,
    Uuid(Option<usize>),
    Enum(Box<[(Value, Py<PyAny>)]>),
    List(Box<Self>),
    Set(Box<Self>),
    Nullable(Box<Self>),
    Dict(Box<Self>),
}

pub struct ParamKernel {
    schema: CompiledSchema,
    shape: Shape,
}

impl std::fmt::Debug for ParamKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParamKernel")
            .field("shape", &self.shape)
            .finish_non_exhaustive()
    }
}

fn plain_json(obj: &Bound<'_, PyAny>) -> Option<Json> {
    if obj.is_none() {
        return Some(Json::Null);
    }
    if let Ok(flag) = obj.cast::<PyBool>() {
        return Some(flag.is_true().into());
    }
    if let Ok(number) = obj.cast::<PyInt>() {
        return number
            .extract::<i64>()
            .map(Into::into)
            .or_else(|_| number.extract::<u64>().map(Into::into))
            .ok();
    }
    if let Ok(number) = obj.cast::<PyFloat>() {
        return Number::from_f64(number.value()).map(Json::Number);
    }
    if let Ok(text) = obj.cast::<PyString>() {
        return Some(text.to_str().ok()?.into());
    }
    let items = obj
        .cast::<PyList>()
        .map(|list| list.iter().collect::<Vec<_>>())
        .or_else(|_| obj.cast::<PyTuple>().map(|tuple| tuple.iter().collect()))
        .ok()?;
    items
        .iter()
        .map(plain_json)
        .collect::<Option<_>>()
        .map(Json::Array)
}

fn allowed(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "str" => &[
            "strict",
            "pattern",
            "min_length",
            "max_length",
            "strip_whitespace",
            "to_upper",
            "to_lower",
        ],
        "int" => &["strict", "multiple_of", "le", "lt", "ge", "gt"],
        "float" => &[
            "strict",
            "allow_inf_nan",
            "multiple_of",
            "le",
            "lt",
            "ge",
            "gt",
        ],
        "bool" | "date" | "time" | "timedelta" => &["strict"],
        "none" => &[],
        "bytes" => &["strict", "min_length", "max_length"],
        "literal" => &["expected"],
        _ => return None,
    })
}

#[derive(Default)]
struct Converter<'py> {
    definitions: Vec<(String, Bound<'py, PyAny>)>,
    active: Vec<String>,
}

impl<'py> Converter<'py> {
    fn nested(
        &mut self,
        dict: &Bound<'py, PyDict>,
        out: &mut Map<String, Json>,
        key: &str,
        in_union: bool,
    ) -> Option<Shape> {
        let Some(inner) = dict.get_item(key).ok()? else {
            return Some(Shape::Plain);
        };
        let (json, shape) = self.convert(&inner, in_union)?;
        out.insert(key.to_owned(), json);
        Some(shape)
    }

    fn definitions(&mut self, dict: &Bound<'py, PyDict>, in_union: bool) -> Option<(Json, Shape)> {
        let py = dict.py();
        for definition in dict
            .get_item(intern!(py, "definitions"))
            .ok()??
            .cast_into::<PyList>()
            .ok()?
            .iter()
        {
            let reference = definition
                .get_item(intern!(py, "ref"))
                .ok()?
                .extract()
                .ok()?;
            self.definitions.push((reference, definition));
        }
        self.convert(&dict.get_item(intern!(py, "schema")).ok()??, in_union)
    }

    fn reference(&mut self, dict: &Bound<'py, PyDict>, in_union: bool) -> Option<(Json, Shape)> {
        let reference: String = dict
            .get_item(intern!(dict.py(), "schema_ref"))
            .ok()??
            .extract()
            .ok()?;
        if self.active.contains(&reference) {
            return None;
        }
        let target = self
            .definitions
            .iter()
            .find(|(name, _)| *name == reference)?
            .1
            .clone();
        self.active.push(reference);
        let converted = self.convert(&target, in_union);
        self.active.pop();
        converted
    }

    fn convert(&mut self, schema: &Bound<'py, PyAny>, in_union: bool) -> Option<(Json, Shape)> {
        let py = schema.py();
        let dict = schema.cast::<PyDict>().ok()?;
        let kind: String = dict.get_item(intern!(py, "type")).ok()??.extract().ok()?;
        match kind.as_str() {
            "definitions" => return self.definitions(dict, in_union),
            "definition-ref" => return self.reference(dict, in_union),
            _ => {}
        }
        let mut out = Map::new();
        out.insert("type".to_owned(), kind.clone().into());
        let (shape, keys): (Shape, &[&str]) = match kind.as_str() {
            "list" | "set" => {
                let inner = self.nested(dict, &mut out, "items_schema", false)?;
                let shape = if kind == "set" {
                    Shape::Set(Box::new(inner))
                } else {
                    Shape::List(Box::new(inner))
                };
                (
                    shape,
                    &["items_schema", "min_length", "max_length", "strict"],
                )
            }
            "nullable" => (
                Shape::Nullable(Box::new(self.nested(dict, &mut out, "schema", in_union)?)),
                &["schema", "strict"],
            ),
            "dict" => {
                if let Some(keys) = dict.get_item(intern!(py, "keys_schema")).ok()? {
                    let keys = keys.cast_into::<PyDict>().ok()?;
                    (keys.len() == 1
                        && keys.get_item("type").ok()??.extract::<&str>().ok()? == "str")
                        .then_some(())?;
                    out.insert("keys_schema".to_owned(), serde_json::json!({"type": "str"}));
                }
                let values = self.nested(dict, &mut out, "values_schema", false)?;
                (
                    Shape::Dict(Box::new(values)),
                    &[
                        "keys_schema",
                        "values_schema",
                        "min_length",
                        "max_length",
                        "strict",
                    ],
                )
            }
            "union" => {
                let choices = dict
                    .get_item(intern!(py, "choices"))
                    .ok()??
                    .cast_into::<PyList>()
                    .ok()?
                    .iter()
                    .map(|choice| match self.convert(&choice, true)? {
                        (json, Shape::Plain) => Some(json),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()?;
                out.insert("choices".to_owned(), Json::Array(choices));
                (Shape::Plain, &["choices", "mode"])
            }
            "enum" => return (!in_union).then(|| enum_schema(dict)).flatten(),
            "datetime" => (Shape::Datetime, &["strict"]),
            "uuid" => return (!in_union).then(|| uuid_schema(dict)).flatten(),
            _ => (Shape::Plain, allowed(&kind)?),
        };
        for (key, value) in dict.iter() {
            let key = key.cast_into::<PyString>().ok()?;
            let key = key.to_str().ok()?;
            if IGNORED.contains(&key) || out.contains_key(key) {
                continue;
            }
            if key == "microseconds_precision" && value.extract::<&str>().ok()? == "truncate" {
                continue;
            }
            if !keys.contains(&key) {
                return None;
            }
            out.insert(key.to_owned(), plain_json(&value)?);
        }
        if in_union && !matches!(shape, Shape::Plain) {
            return None;
        }
        Some((Json::Object(out), shape))
    }
}

fn uuid_schema(dict: &Bound<'_, PyDict>) -> Option<(Json, Shape)> {
    let mut version = None;
    for (key, value) in dict.iter() {
        let key = key.cast_into::<PyString>().ok()?;
        match key.to_str().ok()? {
            "type" => {}
            "version" => version = value.extract::<Option<usize>>().ok()?,
            "strict" if !value.is_truthy().ok()? => {}
            name if IGNORED.contains(&name) => {}
            _ => return None,
        }
    }
    Some((serde_json::json!({ "type": "str" }), Shape::Uuid(version)))
}

fn uuid_to_py<'py>(
    py: Python<'py>,
    text: &str,
    version: Option<usize>,
) -> Option<Bound<'py, PyAny>> {
    let parsed = uuid::Uuid::parse_str(text).ok()?;
    if version.is_some_and(|version| {
        parsed.get_version_num() != version || parsed.get_variant() != uuid::Variant::RFC4122
    }) {
        return None;
    }
    let instance = OBJECT_NEW.get(py).ok()?.call1((UUID.get(py).ok()?,)).ok()?;
    let setattr = OBJECT_SETATTR.get(py).ok()?;
    setattr
        .call1((&instance, intern!(py, "int"), parsed.as_u128()))
        .ok()?;
    setattr
        .call1((
            &instance,
            intern!(py, "is_safe"),
            SAFE_UNKNOWN.get(py).ok()?,
        ))
        .ok()?;
    Some(instance)
}

fn enum_schema(dict: &Bound<'_, PyDict>) -> Option<(Json, Shape)> {
    let py = dict.py();
    for (key, value) in dict.iter() {
        let key = key.cast_into::<PyString>().ok()?;
        match key.to_str().ok()? {
            "type" | "cls" | "members" | "sub_type" => {}
            "missing" if value.is_none() => {}
            name if IGNORED.contains(&name) => {}
            _ => return None,
        }
    }
    let members = dict
        .get_item(intern!(py, "members"))
        .ok()??
        .cast_into::<PyList>()
        .ok()?;
    let sub_type = dict
        .get_item(intern!(py, "sub_type"))
        .ok()?
        .map(|kind| kind.extract::<String>())
        .transpose()
        .ok()?;
    let mut values = Vec::with_capacity(members.len());
    let mut lookup = Vec::with_capacity(members.len());
    for member in members.iter() {
        let value = plain_json(&member.getattr(intern!(py, "value")).ok()?)?;
        let kernel = serde_to_value(&value);
        let fits = match sub_type.as_deref() {
            None => matches!(kernel, Value::Str(_) | Value::Int(_) | Value::Bool(_)),
            Some("str") => matches!(kernel, Value::Str(_)),
            Some("int") => matches!(kernel, Value::Int(_)),
            Some(_) => false,
        };
        fits.then_some(())?;
        values.push(value);
        lookup.push((kernel, member.unbind()));
    }
    let schema = match sub_type {
        Some(kind) => serde_json::json!({ "type": kind }),
        None => serde_json::json!({ "type": "enum", "members": values }),
    };
    Some((schema, Shape::Enum(lookup.into_boxed_slice())))
}

impl ParamKernel {
    pub fn compile(core_schema: &Bound<'_, PyAny>) -> Option<Self> {
        let py = core_schema.py();
        let mut schema = core_schema.clone();
        while let Ok(dict) = schema.cast::<PyDict>()
            && dict
                .get_item(intern!(py, "type"))
                .ok()
                .flatten()
                .is_some_and(|kind| kind.extract::<&str>().is_ok_and(|kind| kind == "default"))
        {
            schema = dict.get_item(intern!(py, "schema")).ok()??;
        }
        let (json, shape) = Converter::default().convert(&schema, false)?;
        let schema: CoreSchema = serde_json::from_value(json).ok()?;
        Some(Self {
            schema: CompiledSchema::new(&schema, None).ok()?,
            shape,
        })
    }

    #[must_use]
    pub fn validate<'py>(&self, py: Python<'py>, input: &Value) -> Option<Bound<'py, PyAny>> {
        if self.shape.has_timestamp(input) {
            return None;
        }
        let value = self.schema.validate_value(input).ok()?;
        self.shape.to_py(py, &value)
    }

    #[must_use]
    pub fn validate_str<'py>(&self, py: Python<'py>, input: &str) -> Option<Bound<'py, PyAny>> {
        self.validate(py, &Value::Str(input.to_owned()))
    }

    #[must_use]
    pub fn validate_strs<'py, S: AsRef<str>>(
        &self,
        py: Python<'py>,
        inputs: &[S],
    ) -> Option<Bound<'py, PyAny>> {
        let items = inputs
            .iter()
            .map(|input| Value::Str(input.as_ref().to_owned()))
            .collect();
        self.validate(py, &Value::Array(items))
    }

    #[must_use]
    pub fn validate_json<'py>(
        &self,
        py: Python<'py>,
        input: &OwnedValue,
    ) -> Option<Bound<'py, PyAny>> {
        self.validate(py, &json_value(input)?)
    }
}

fn json_value(value: &OwnedValue) -> Option<Value> {
    Some(match value {
        OwnedValue::Static(StaticNode::Null) => Value::Null,
        OwnedValue::Static(StaticNode::Bool(flag)) => Value::Bool(*flag),
        OwnedValue::Static(StaticNode::I64(number)) => Value::Int(Int::I64(*number)),
        OwnedValue::Static(StaticNode::U64(number)) => {
            Value::Int(Int::I64(i64::try_from(*number).ok()?))
        }
        OwnedValue::Static(StaticNode::F64(number)) => Value::Float(*number),
        OwnedValue::String(text) => Value::Str(text.clone()),
        OwnedValue::Array(items) => {
            Value::Array(items.iter().map(json_value).collect::<Option<_>>()?)
        }
        OwnedValue::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(key, item)| Some((key.clone(), json_value(item)?)))
                .collect::<Option<_>>()?,
        ),
    })
}

impl Shape {
    fn has_timestamp(&self, input: &Value) -> bool {
        match (self, input) {
            (Self::Datetime, Value::Int(_) | Value::Float(_)) => true,
            (Self::Datetime, Value::Str(text)) => text.trim().parse::<f64>().is_ok(),
            (Self::List(inner) | Self::Set(inner) | Self::Nullable(inner), Value::Array(items)) => {
                items.iter().any(|item| inner.has_timestamp(item))
            }
            (Self::Nullable(inner), _) => inner.has_timestamp(input),
            (Self::Dict(inner), Value::Object(entries)) => {
                entries.iter().any(|(_, item)| inner.has_timestamp(item))
            }
            _ => false,
        }
    }

    fn to_py<'py>(&self, py: Python<'py>, value: &Value) -> Option<Bound<'py, PyAny>> {
        match (self, value) {
            (Self::Plain | Self::Datetime, _) => {
                value_to_py(py, value).map(|value| value.into_bound(py))
            }
            (Self::Uuid(version), Value::Str(text)) => uuid_to_py(py, text, *version),
            (Self::Enum(members), _) => members
                .iter()
                .find(|(member, _)| member == value)
                .map(|(_, member)| member.bind(py).clone()),
            (Self::Nullable(_), Value::Null) => Some(py.None().into_bound(py)),
            (Self::Nullable(inner), _) => inner.to_py(py, value),
            (Self::List(inner), Value::Array(items)) => {
                let items = items
                    .iter()
                    .map(|item| inner.to_py(py, item))
                    .collect::<Option<Vec<_>>>()?;
                PyList::new(py, items).ok().map(Bound::into_any)
            }
            (Self::Set(inner), Value::Array(items)) => {
                let items = items
                    .iter()
                    .map(|item| inner.to_py(py, item))
                    .collect::<Option<Vec<_>>>()?;
                PySet::new(py, items).ok().map(Bound::into_any)
            }
            (Self::Dict(inner), Value::Object(entries)) => {
                let dict = PyDict::new(py);
                for (key, item) in entries {
                    dict.set_item(key, inner.to_py(py, item)?).ok()?;
                }
                Some(dict.into_any())
            }
            _ => None,
        }
    }
}
