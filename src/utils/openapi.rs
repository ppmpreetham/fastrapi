use crate::ffi::pydantic;
use crate::routing::types::{ParameterConstraints, ParameterSource, RouteHandler};
use super::utils::py_dict_to_json;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use std::collections::HashMap;
use tracing::debug;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenApiSpec {
    pub openapi: String,
    pub info: OpenApiInfo,
    pub paths: HashMap<String, PathItem>,
    pub components: Option<Components>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenApiInfo {
    pub title: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub get: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub put: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delete: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Vec<Parameter>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Vec<Parameter>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<RequestBody>,
    pub responses: HashMap<String, Response>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Parameter {
    pub name: String,
    #[serde(rename = "in")]
    pub location: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestBody {
    pub required: bool,
    pub content: HashMap<String, MediaType>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaType {
    pub schema: JsonValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<HashMap<String, MediaType>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Components {
    pub schemas: HashMap<String, JsonValue>,
}

impl Default for OpenApiSpec {
    fn default() -> Self {
        Self {
            openapi: "3.0.0".to_string(),
            info: OpenApiInfo {
                title: "FastrAPI".to_string(),
                version: "0.1.0".to_string(),
                description: Some("API built with FastrAPI".to_string()),
            },
            paths: HashMap::new(),
            components: Some(Components {
                schemas: HashMap::new(),
            }),
        }
    }
}

pub fn extract_pydantic_schema(py: Python, model: &Bound<PyAny>) -> Option<JsonValue> {
    // Pydantic v2
    if let Ok(schema_method) = model.getattr("model_json_schema") {
        if let Ok(result) = schema_method.call0() {
            if let Ok(dict) = result.cast::<PyDict>() {
                return Some(py_dict_to_json(py, dict));
            }
        }
    }

    // Pydantic v1
    if let Ok(schema_method) = model.getattr("schema") {
        if let Ok(result) = schema_method.call0() {
            if let Ok(dict) = result.cast::<PyDict>() {
                return Some(py_dict_to_json(py, dict));
            }
        }
    }

    None
}

fn get_schema_name(model: &Bound<PyAny>) -> String {
    model
        .getattr("__name__")
        .ok()
        .and_then(|name| name.extract::<String>().ok())
        .unwrap_or_else(|| "UnknownSchema".to_string())
}

fn python_type_to_openapi_type(py: Python, type_hint: &Bound<PyAny>) -> JsonValue {
    if let Ok(type_str) = type_hint.str() {
        let type_name = type_str.to_string_lossy().to_string();

        if type_name.contains("str") {
            return json!({"type": "string"});
        } else if type_name.contains("int") {
            return json!({"type": "integer"});
        } else if type_name.contains("float") {
            return json!({"type": "number"});
        } else if type_name.contains("bool") {
            return json!({"type": "boolean"});
        } else if type_name.contains("List") || type_name.contains("list") {
            return json!({"type": "array", "items": {"type": "string"}});
        } else if type_name.contains("Dict") || type_name.contains("dict") {
            return json!({"type": "object"});
        }
    }

    if pydantic::is_pydantic_model(py, type_hint) {
        let schema_name = get_schema_name(type_hint);
        return json!({"$ref": format!("#/components/schemas/{}", schema_name)});
    }

    json!({"type": "string"})
}

fn apply_parameter_constraints(mut schema: JsonValue, constraints: &ParameterConstraints) -> JsonValue {
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

pub fn build_openapi_spec(
    py: Python<'_>,
    routes: &papaya::HashMap<String, std::sync::Arc<RouteHandler>>,
    title: &str,
    version: &str,
    description: &str,
) -> serde_json::Value {
    let mut spec = OpenApiSpec::default();
    spec.info.title = title.to_string();
    spec.info.version = version.to_string();
    spec.info.description = if description.is_empty() {
        None
    } else {
        Some(description.to_string())
    };

    let mut schemas: HashMap<String, JsonValue> = HashMap::new();
    let guard = routes.guard();

    for (route_key, handler) in routes.iter(&guard) {
        let handler = handler.as_ref();
        let parts: Vec<&str> = route_key.splitn(2, ' ').collect();
        if parts.len() != 2 {
            continue;
        }

        let method = parts[0].to_lowercase();
        let path = parts[1].to_string();

        let description = handler
            .func
            .bind(py)
            .getattr("__doc__")
            .ok()
            .and_then(|doc| doc.extract::<String>().ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let mut operation = Operation {
            summary: Some(format!("{} {}", method.to_uppercase(), path)),
            description,
            tags: None,
            parameters: None,
            request_body: None,
            responses: HashMap::new(),
        };

        let mut parameters = Vec::new();

        for param in &handler.parsed_params {
            let location = match param.source {
                ParameterSource::Path => "path",
                ParameterSource::Query => "query",
                ParameterSource::Header => "header",
                ParameterSource::Cookie => "cookie",
                ParameterSource::Body => continue,
            };

            let schema = param
                .annotation
                .as_ref()
                .map(|annotation| python_type_to_openapi_type(py, annotation.bind(py)))
                .unwrap_or_else(|| json!({"type": "string"}));

            parameters.push(Parameter {
                name: param.external_name.clone(),
                location: location.to_string(),
                required: Some(param.required || location == "path"),
                schema: Some(apply_parameter_constraints(schema, &param.constraints)),
                description: param.description.clone(),
            });
        }

        if !parameters.is_empty() {
            operation.parameters = Some(parameters);
        }

        // Request body for POST/PUT/PATCH with validators
        if !handler.param_validators.is_empty()
            && ["post", "put", "patch"].contains(&method.as_str())
        {
            let validator_count = handler.param_validators.len();

            if validator_count == 1 {
                let (_param_name, validator) = &handler.param_validators[0];
                let validator_bound = validator.bind(py);

                if let Some(schema) = extract_pydantic_schema(py, validator_bound) {
                    let schema_name = get_schema_name(validator_bound);
                    schemas.insert(schema_name.clone(), schema.clone());

                    operation.request_body = Some(RequestBody {
                        required: true,
                        content: {
                            let mut content = HashMap::new();
                            content.insert(
                                "application/json".to_string(),
                                MediaType {
                                    schema: json!({
                                        "$ref": format!("#/components/schemas/{}", schema_name)
                                    }),
                                },
                            );
                            content
                        },
                    });
                }
            } else if validator_count > 1 {
                let mut properties = serde_json::Map::new();
                let mut required_fields = Vec::new();

                for (param_name, validator) in &handler.param_validators {
                    let validator_bound = validator.bind(py);

                    if let Some(schema) = extract_pydantic_schema(py, validator_bound) {
                        let schema_name = get_schema_name(validator_bound);
                        schemas.insert(schema_name.clone(), schema);
                        properties.insert(
                            param_name.clone(),
                            json!({ "$ref": format!("#/components/schemas/{}", schema_name) }),
                        );
                        required_fields.push(param_name.clone());
                    }
                }

                if !properties.is_empty() {
                    let func_name = handler
                        .func
                        .bind(py)
                        .getattr("__name__")
                        .ok()
                        .and_then(|n| n.extract::<String>().ok())
                        .unwrap_or_else(|| "unknown".to_string());

                    // "/register" -> "register"
                    // "/users/{id}" -> "users__id_"
                    let path_slug = path
                        .trim_start_matches('/')
                        .replace('/', "__")
                        .replace('{', "_")
                        .replace('}', "_");

                    let wrapper_name = format!("Body_{}_{}_{}", func_name, path_slug, method);

                    schemas.insert(
                        wrapper_name.clone(),
                        json!({
                            "type": "object",
                            "properties": properties,
                            "required": required_fields,
                        }),
                    );

                    operation.request_body = Some(RequestBody {
                        required: true,
                        content: {
                            let mut content = HashMap::new();
                            content.insert(
                                "application/json".to_string(),
                                MediaType {
                                    schema: json!({
                                        "$ref": format!("#/components/schemas/{}", wrapper_name)
                                    }),
                                },
                            );
                            content
                        },
                    });
                }
            }
        }
        // Default 200 response
        operation.responses.insert(
            "200".to_string(),
            Response {
                description: "Successful Response".to_string(),
                content: Some({
                    let mut content = HashMap::new();
                    content.insert(
                        "application/json".to_string(),
                        MediaType {
                            schema: json!({"type": "object"}),
                        },
                    );
                    content
                }),
            },
        );

        // 422 for validation errors
        if !handler.param_validators.is_empty() || !handler.parsed_params.is_empty() {
            operation.responses.insert(
                "422".to_string(),
                Response {
                    description: "Validation Error".to_string(),
                    content: Some({
                        let mut content = HashMap::new();
                        content.insert(
                            "application/json".to_string(),
                            MediaType {
                                schema: json!({
                                    "type": "object",
                                    "properties": {
                                        "detail": {"type": "string"}
                                    }
                                }),
                            },
                        );
                        content
                    }),
                },
            );
        }

        let path_item = spec.paths.entry(path).or_insert_with(|| PathItem {
            get: None,
            post: None,
            put: None,
            delete: None,
            patch: None,
            parameters: None,
        });

        match method.as_str() {
            "get" => path_item.get = Some(operation),
            "post" => path_item.post = Some(operation),
            "put" => path_item.put = Some(operation),
            "delete" => path_item.delete = Some(operation),
            "patch" => path_item.patch = Some(operation),
            _ => {}
        }
    }

    if let Some(components) = &mut spec.components {
        components.schemas = schemas;
    }
    debug!("Built OpenAPI spec with {} paths", spec.paths.len());
    serde_json::to_value(spec).unwrap_or_else(|_| json!({}))
}
