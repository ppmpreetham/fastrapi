use fastrapi_pyutil::json::py_any_to_json;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};
use simd_json::json;
use simd_json::owned::Value as JsonValue;
use simd_json::prelude::{ValueAsMutObject, ValueAsScalar, ValueObjectAccess};
use std::collections::HashMap;

const MARKER: &str = "$fastrapi_schema";
const REF_TEMPLATE: &str = "#/components/schemas/{model}";

struct Field {
    annotation: Py<PyAny>,
    mode: &'static str,
    title: Option<String>,
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
        let index = self.fields.len();
        self.fields.push(Field {
            annotation: annotation.clone().unbind(),
            mode,
            title,
        });
        json!({ MARKER: index })
    }

    pub(crate) fn resolve(
        &self,
        py: Python<'_>,
        separate_input_output_schemas: bool,
        schemas: &mut HashMap<String, JsonValue>,
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
        schemas: &mut HashMap<String, JsonValue>,
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
            if let Some(title) = &self.fields[index].title
                && schema.get("$ref").is_none()
                && let Some(object) = schema.as_object_mut()
            {
                object.insert("title".to_string(), json!(title.as_str()));
            }
            values.push((index, schema));
        }
        Ok(values)
    }
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
