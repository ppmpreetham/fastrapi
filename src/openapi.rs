// src/openapi.rs

use crate::utils::py_dict_to_json;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use std::collections::HashMap;
use tracing::debug;

use papaya::HashMap as PapayaMap;

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<RequestBody>,
    pub responses: HashMap<String, Response>,
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

/// Extract Pydantic model's JSON schema
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

/// Get schema name from Pydantic model (fallback to UnknownSchema)
fn get_schema_name(model: &Bound<PyAny>) -> String {
    model
        .getattr("__name__")
        .ok()
        .and_then(|name| name.extract::<String>().ok())
        .unwrap_or_else(|| "UnknownSchema".to_string())
}

/// Build OpenAPI spec from registered routes
pub fn build_openapi_spec(
    py: Python,
    routes: &PapayaMap<String, crate::RouteHandler>,
) -> OpenApiSpec {
    let mut spec = OpenApiSpec::default();
    let mut schemas: HashMap<String, JsonValue> = HashMap::new();
    let guard = routes.guard();

    for (route_key, handler) in routes.iter(&guard) {
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
            request_body: None,
            responses: HashMap::new(),
        };

        // --- FIXED BODY SCHEMA GENERATION ---
        if !handler.param_validators.is_empty()
            && ["post", "put", "patch"].contains(&method.as_str())
        {
            let content_schema = if handler.param_validators.len() == 1 {
                // CASE A: Single Body -> Direct Reference
                let (_, validator) = &handler.param_validators[0];
                let validator_bound = validator.bind(py);

                if let Some(schema) = extract_pydantic_schema(py, validator_bound) {
                    let schema_name = get_schema_name(validator_bound);
                    schemas.insert(schema_name.clone(), schema);
                    json!({ "$ref": format!("#/components/schemas/{}", schema_name) })
                } else {
                    json!({ "type": "object" })
                }
            } else {
                // CASE B: Multiple Bodies -> Nested Properties
                let mut properties = HashMap::new();
                for (param_name, validator) in &handler.param_validators {
                    let validator_bound = validator.bind(py);
                    if let Some(schema) = extract_pydantic_schema(py, validator_bound) {
                        let schema_name = get_schema_name(validator_bound);
                        schemas.insert(schema_name.clone(), schema);
                        properties.insert(
                            param_name.clone(),
                            json!({ "$ref": format!("#/components/schemas/{}", schema_name) }),
                        );
                    }
                }
                json!({ "type": "object", "properties": properties })
            };

            operation.request_body = Some(RequestBody {
                required: true,
                content: {
                    let mut map = HashMap::new();
                    map.insert(
                        "application/json".to_string(),
                        MediaType {
                            schema: content_schema,
                        },
                    );
                    map
                },
            });
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
        if !handler.param_validators.is_empty() {
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
    spec
}
