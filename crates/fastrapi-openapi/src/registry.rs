use fastrapi_pyutil::json::py_any_to_json;
use fastrapi_routing::route::SchemaAliases;
use indexmap::IndexMap;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};
use simd_json::json;
use simd_json::owned::Object;
use simd_json::owned::Value as JsonValue;
use simd_json::prelude::{ValueAsMutObject, ValueAsScalar, ValueObjectAccess};

const MARKER: &str = "$fastrapi_schema";
const REF_TEMPLATE: &str = "#/components/schemas/{model}";

struct Field {
    annotation: Py<PyAny>,
    mode: &'static str,
    title: Option<String>,
    default: Option<JsonValue>,
    form_aliases: Option<SchemaAliases>,
}

#[derive(Default)]
pub(crate) struct SchemaRegistry {
    fields: Vec<Field>,
}

impl SchemaRegistry {
    pub(crate) fn field(
        &mut self,
        annotation: &Bound<'_, PyAny>,
        mode: &'static str,
        title: Option<String>,
    ) -> JsonValue {
        self.field_with_default(annotation, mode, title, None)
    }

    pub(crate) fn field_with_default(
        &mut self,
        annotation: &Bound<'_, PyAny>,
        mode: &'static str,
        title: Option<String>,
        default: Option<JsonValue>,
    ) -> JsonValue {
        self.push(Field {
            annotation: annotation.clone().unbind(),
            mode,
            title,
            default,
            form_aliases: None,
        })
    }

    /// A form model's schema, renamed to its request names like FastAPI's `_get_form_model_schema`.
    pub(crate) fn form_model(
        &mut self,
        annotation: &Bound<'_, PyAny>,
        title: Option<String>,
        aliases: Option<&SchemaAliases>,
    ) -> JsonValue {
        self.push(Field {
            annotation: annotation.clone().unbind(),
            mode: "validation",
            title,
            default: None,
            form_aliases: aliases.cloned(),
        })
    }

    fn push(&mut self, field: Field) -> JsonValue {
        let index = self.fields.len();
        self.fields.push(field);
        json!({ MARKER: index })
    }

    pub(crate) fn resolve(
        &self,
        py: Python<'_>,
        separate_input_output_schemas: bool,
        schemas: &mut IndexMap<String, JsonValue>,
    ) -> Vec<JsonValue> {
        let indices: Vec<usize> = (0..self.fields.len()).collect();
        let mut resolved = vec![json!({}); self.fields.len()];
        if indices.is_empty() {
            return resolved;
        }
        match self.generate(py, &indices, separate_input_output_schemas, schemas) {
            Ok(values) => {
                for (index, value) in values {
                    resolved[index] = value;
                }
            }
            Err(error) => {
                tracing::debug!("OpenAPI schema generation fell back to single fields: {error}");
                for index in indices {
                    if let Ok(values) =
                        self.generate(py, &[index], separate_input_output_schemas, schemas)
                    {
                        for (index, value) in values {
                            resolved[index] = value;
                        }
                    }
                }
            }
        }
        resolved
    }

    fn generate(
        &self,
        py: Python<'_>,
        indices: &[usize],
        separate_input_output_schemas: bool,
        schemas: &mut IndexMap<String, JsonValue>,
    ) -> PyResult<Vec<(usize, JsonValue)>> {
        let kwargs = PyDict::new(py);
        kwargs.set_item("ref_template", REF_TEMPLATE)?;
        let generator = py
            .import("pydantic.json_schema")?
            .getattr("GenerateJsonSchema")?
            .call((), Some(&kwargs))?;
        let type_adapter = py.import("pydantic")?.getattr("TypeAdapter")?;
        let inputs = PyList::empty(py);
        for &index in indices {
            let field = &self.fields[index];
            let mode = if separate_input_output_schemas {
                field.mode
            } else {
                "validation"
            };
            let adapter_kwargs = PyDict::new(py);
            adapter_kwargs.set_item("_parent_depth", 0)?;
            let core_schema = type_adapter
                .call((field.annotation.bind(py),), Some(&adapter_kwargs))?
                .getattr("core_schema")?;
            inputs.append((index, mode, core_schema))?;
        }
        let kwargs = PyDict::new(py);
        kwargs.set_item("inputs", inputs)?;
        let result = generator.call_method("generate_definitions", (), Some(&kwargs))?;
        let result = result.cast::<PyTuple>()?;
        let mapping = result.get_item(0)?;
        let mapping = mapping.cast::<PyDict>()?;
        let definitions = result.get_item(1)?;
        let definitions = definitions.cast::<PyDict>()?;
        for (name, definition) in definitions.iter() {
            let mut definition = py_any_to_json(py, &definition);
            trim_description(&mut definition);
            schemas.insert(name.extract::<String>()?, definition);
        }
        let mut values = Vec::with_capacity(indices.len());
        for (key, schema) in mapping.iter() {
            let (index, _mode): (usize, String) = key.extract()?;
            let mut schema = py_any_to_json(py, &schema);
            let field = &self.fields[index];
            if let Some(object) = schema.as_object_mut() {
                if let Some(title) = &field.title
                    && !object.contains_key("$ref")
                {
                    object.insert("title".to_string(), json!(title.as_str()));
                }
                if let Some(default) = &field.default {
                    object.insert("default".to_string(), default.clone());
                }
            }
            if let Some(aliases) = &field.form_aliases {
                schema = renamed_form_model(schema, aliases, schemas);
            }
            values.push((index, schema));
        }
        Ok(values)
    }
}

fn renamed_form_model(
    schema: JsonValue,
    aliases: &SchemaAliases,
    schemas: &IndexMap<String, JsonValue>,
) -> JsonValue {
    let Some(mut renamed) = schema
        .get("$ref")
        .and_then(ValueAsScalar::as_str)
        .and_then(|reference| reference.strip_prefix("#/components/schemas/"))
        .and_then(|name| schemas.get(name))
        .cloned()
    else {
        return schema;
    };
    let Some(object) = renamed.as_object_mut() else {
        return schema;
    };
    let names = |name: &str| {
        aliases
            .iter()
            .find(|(alias, _)| alias == name)
            .map(|(_, names)| &**names)
    };
    if let Some(JsonValue::Object(properties)) = object.remove("properties") {
        let properties: Object = properties
            .into_iter()
            .map(|(name, value)| {
                (
                    names(&name).map_or_else(|| name.clone(), |names| names[0].clone()),
                    value,
                )
            })
            .collect();
        object.insert("properties".to_string(), JsonValue::from(properties));
    }
    let mut required = Vec::new();
    let mut choices = Vec::new();
    if let Some(JsonValue::Array(listed)) = object.remove("required") {
        for name in listed.iter().filter_map(ValueAsScalar::as_str) {
            match names(name) {
                Some(names) if names.len() > 1 => {
                    let any_of: Vec<JsonValue> = names
                        .iter()
                        .map(|name| json!({ "required": [name] }))
                        .collect();
                    choices.push(json!({ "anyOf": any_of }));
                }
                Some(names) => required.push(json!(names[0].as_str())),
                None => required.push(json!(name)),
            }
        }
    }
    object.insert("required".to_string(), JsonValue::Array(Box::new(required)));
    if !choices.is_empty() {
        match object.get_mut("allOf") {
            Some(JsonValue::Array(all_of)) => all_of.extend(choices),
            _ => {
                object.insert("allOf".to_string(), JsonValue::Array(Box::new(choices)));
            }
        }
    }
    renamed
}

fn trim_description(definition: &mut JsonValue) {
    let Some(description) = definition
        .get("description")
        .and_then(ValueAsScalar::as_str)
        .map(str::to_owned)
    else {
        return;
    };
    if let Some(cut) = description.split('\u{c}').next()
        && cut.len() != description.len()
        && let Some(object) = definition.as_object_mut()
    {
        object.insert("description".to_string(), json!(cut));
    }
}

pub(crate) fn substitute(value: &mut JsonValue, resolved: &[JsonValue]) {
    match value {
        JsonValue::Object(object) => {
            if object.len() == 1
                && let Some(index) = object.get(MARKER).and_then(ValueAsScalar::as_usize)
            {
                *value = resolved.get(index).cloned().unwrap_or_else(|| json!({}));
                return;
            }
            for (_, child) in object.iter_mut() {
                substitute(child, resolved);
            }
        }
        JsonValue::Array(items) => {
            for child in items.iter_mut() {
                substitute(child, resolved);
            }
        }
        JsonValue::Static(_) | JsonValue::String(_) => {}
    }
}
