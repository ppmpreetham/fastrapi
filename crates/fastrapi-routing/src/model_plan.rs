use crate::kernel_params::plain_json;
use crate::kernel_validation::value_to_py;
use pydantic_core_kernel::Value;
use pydantic_core_kernel::json::serde_to_value;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PySet, PyString};
use smallvec::SmallVec;

#[derive(Debug)]
enum Plan {
    Plain,
    Model(Box<Model>),
    Enum(Box<[(Value, Py<PyAny>)]>),
    List(Box<Self>),
    Set(Box<Self>),
    Nullable(Box<Self>),
    Dict(Box<Self>),
    Ref(usize),
}

#[derive(Debug)]
struct Model {
    construct: Py<PyAny>,
    fields: Box<[Field]>,
}

#[derive(Debug)]
struct Field {
    name: Box<str>,
    name_py: Py<PyString>,
    keys: SmallVec<[Box<str>; 2]>,
    plan: Plan,
}

#[derive(Debug)]
pub struct ModelPlan {
    root: Plan,
    definitions: Box<[Plan]>,
}

fn schema_kind<'py>(schema: &Bound<'py, PyAny>) -> Option<(Bound<'py, PyDict>, String)> {
    let dict = schema.cast::<PyDict>().ok()?.clone();
    let kind = dict
        .get_item(intern!(schema.py(), "type"))
        .ok()??
        .extract()
        .ok()?;
    Some((dict, kind))
}

fn item<'py>(dict: &Bound<'py, PyDict>, key: &str) -> Option<Bound<'py, PyAny>> {
    dict.get_item(key)
        .ok()
        .flatten()
        .filter(|value| !value.is_none())
}

fn flag(dict: &Bound<'_, PyDict>, key: &str) -> bool {
    item(dict, key).is_some_and(|value| value.is_truthy().unwrap_or(true))
}

fn needs_plan(value: &Bound<'_, PyAny>) -> bool {
    if let Ok(dict) = value.cast::<PyDict>() {
        let nested = dict.get_item("type").ok().flatten().is_some_and(|kind| {
            kind.extract::<&str>().is_ok_and(|kind| {
                matches!(
                    kind,
                    "model" | "definition-ref" | "dataclass" | "enum" | "set" | "frozenset"
                )
            })
        });
        return nested || dict.values().iter().any(|value| needs_plan(&value));
    }
    value
        .cast::<PyList>()
        .is_ok_and(|list| list.iter().any(|value| needs_plan(&value)))
}

struct Builder {
    names: Vec<String>,
}

impl Builder {
    fn node(&self, schema: &Bound<'_, PyAny>) -> Option<Plan> {
        let (dict, kind) = schema_kind(schema)?;
        let child =
            |key: &str| item(&dict, key).map_or(Some(Plan::Plain), |inner| self.node(&inner));
        Some(match kind.as_str() {
            "model" => Plan::Model(Box::new(self.model(&dict)?)),
            "definition-ref" => {
                let name: String = item(&dict, "schema_ref")?.extract().ok()?;
                Plan::Ref(self.names.iter().position(|known| *known == name)?)
            }
            "nullable" => Plan::Nullable(Box::new(child("schema")?)),
            "list" => Plan::List(Box::new(child("items_schema")?)),
            "set" => match child("items_schema")? {
                inner @ (Plan::Plain | Plan::Enum(_)) => Plan::Set(Box::new(inner)),
                _ => return None,
            },
            "enum" => Plan::Enum(enum_members(&dict)?),
            "dict" => {
                if item(&dict, "keys_schema").is_some_and(|keys| needs_plan(&keys)) {
                    return None;
                }
                Plan::Dict(Box::new(child("values_schema")?))
            }
            "default" | "with-default" if !flag(&dict, "validate_default") => child("schema")?,
            "definitions" | "default" | "with-default" => return None,
            _ if dict.values().iter().any(|value| needs_plan(&value)) => return None,
            _ => Plan::Plain,
        })
    }

    fn model(&self, dict: &Bound<'_, PyDict>) -> Option<Model> {
        let py = dict.py();
        if flag(dict, "custom_init") || flag(dict, "root_model") {
            return None;
        }
        let config = item(dict, "config");
        let config = config
            .as_ref()
            .and_then(|config| config.cast::<PyDict>().ok());
        let setting = |key: &str, default: bool| {
            config
                .and_then(|config| item(config, key))
                .map_or(Some(default), |value| value.is_truthy().ok())
        };
        if config
            .and_then(|config| item(config, "extra_fields_behavior"))
            .is_some_and(|extra| extra.extract::<&str>().is_ok_and(|extra| extra == "allow"))
        {
            return None;
        }
        let by_alias = setting("validate_by_alias", true)?;
        let by_name = setting("validate_by_name", false)?;
        let (inner, inner_kind) = schema_kind(&item(dict, "schema")?)?;
        if inner_kind != "model-fields" {
            return None;
        }
        let fields = item(&inner, "fields")?
            .cast_into::<PyDict>()
            .ok()?
            .iter()
            .map(|(name, field)| {
                let name: String = name.extract().ok()?;
                let (field, field_kind) = schema_kind(&field)?;
                if field_kind != "model-field" {
                    return None;
                }
                let alias = item(&field, "validation_alias")
                    .map(|alias| alias.extract::<String>())
                    .transpose()
                    .ok()?;
                let keys = match alias {
                    Some(alias) => [by_alias.then_some(alias), by_name.then(|| name.clone())]
                        .into_iter()
                        .flatten()
                        .map(String::into_boxed_str)
                        .collect(),
                    None => SmallVec::from_elem(name.clone().into_boxed_str(), 1),
                };
                Some(Field {
                    name_py: PyString::intern(py, &name).unbind(),
                    name: name.into_boxed_str(),
                    keys,
                    plan: self.node(&item(&field, "schema")?)?,
                })
            })
            .collect::<Option<Box<[Field]>>>()?;
        let construct = item(dict, "cls")?
            .getattr(intern!(py, "model_construct"))
            .ok()?
            .unbind();
        Some(Model { construct, fields })
    }
}

fn enum_members(dict: &Bound<'_, PyDict>) -> Option<Box<[(Value, Py<PyAny>)]>> {
    if item(dict, "missing").is_some() {
        return None;
    }
    item(dict, "members")?
        .cast_into::<PyList>()
        .ok()?
        .iter()
        .map(|member| {
            let value = member.getattr(intern!(dict.py(), "value")).ok()?;
            let value = serde_to_value(&plain_json(&value)?);
            matches!(value, Value::Str(_)).then(|| (value, member.unbind()))
        })
        .collect()
}

fn entry<'v>(entries: &'v [(String, Value)], key: &str) -> Option<&'v Value> {
    entries
        .iter()
        .rev()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}

impl ModelPlan {
    #[must_use]
    pub fn compile(core_schema: &Bound<'_, PyAny>) -> Option<Self> {
        let (dict, kind) = schema_kind(core_schema)?;
        if kind != "definitions" {
            let builder = Builder { names: Vec::new() };
            return Some(Self {
                root: builder.node(core_schema)?,
                definitions: Box::new([]),
            });
        }
        let definitions = item(&dict, "definitions")?.cast_into::<PyList>().ok()?;
        let names = definitions
            .iter()
            .map(|definition| {
                item(definition.cast::<PyDict>().ok()?, "ref")?
                    .extract()
                    .ok()
            })
            .collect::<Option<Vec<String>>>()?;
        let builder = Builder { names };
        Some(Self {
            definitions: definitions
                .iter()
                .map(|definition| builder.node(&definition))
                .collect::<Option<_>>()?,
            root: builder.node(&item(&dict, "schema")?)?,
        })
    }

    #[must_use]
    pub fn construct<'py>(
        &self,
        py: Python<'py>,
        input: &Value,
        output: &Value,
    ) -> Option<Bound<'py, PyAny>> {
        self.build(py, &self.root, input, output)
    }

    fn build<'py>(
        &self,
        py: Python<'py>,
        plan: &Plan,
        input: &Value,
        output: &Value,
    ) -> Option<Bound<'py, PyAny>> {
        match (plan, input, output) {
            (Plan::Plain, _, _) => value_to_py(py, output).map(|value| value.into_bound(py)),
            (Plan::Ref(index), _, _) => {
                self.build(py, self.definitions.get(*index)?, input, output)
            }
            (Plan::Enum(members), _, _) => members
                .iter()
                .find(|(member, _)| member == output)
                .map(|(_, member)| member.bind(py).clone()),
            (Plan::Set(inner), _, Value::Array(outputs)) => {
                let items = outputs
                    .iter()
                    .map(|output| self.build(py, inner, output, output))
                    .collect::<Option<Vec<_>>>()?;
                PySet::new(py, items).ok().map(Bound::into_any)
            }
            (Plan::Nullable(_), _, Value::Null) => Some(py.None().into_bound(py)),
            (Plan::Nullable(inner), _, _) => self.build(py, inner, input, output),
            (Plan::List(inner), Value::Array(inputs), Value::Array(outputs))
                if inputs.len() == outputs.len() =>
            {
                let items = inputs
                    .iter()
                    .zip(outputs)
                    .map(|(input, output)| self.build(py, inner, input, output))
                    .collect::<Option<Vec<_>>>()?;
                PyList::new(py, items).ok().map(Bound::into_any)
            }
            (Plan::Dict(inner), Value::Object(inputs), Value::Object(outputs)) => {
                let dict = PyDict::new(py);
                for (key, output) in outputs {
                    let value = self.build(py, inner, entry(inputs, key)?, output)?;
                    dict.set_item(key, value).ok()?;
                }
                Some(dict.into_any())
            }
            (Plan::Model(model), Value::Object(inputs), Value::Object(outputs)) => {
                let kwargs = PyDict::new(py);
                for field in &model.fields {
                    let Some(input) = field.keys.iter().find_map(|key| entry(inputs, key)) else {
                        continue;
                    };
                    let value = self.build(py, &field.plan, input, entry(outputs, &field.name)?)?;
                    kwargs.set_item(field.name_py.bind(py), value).ok()?;
                }
                model.construct.bind(py).call((), Some(&kwargs)).ok()
            }
            _ => None,
        }
    }
}
