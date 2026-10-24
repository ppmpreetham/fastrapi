//! The serde types of an OpenAPI 3.1 document.
//! It also contains a function that merges two JSON values deeply.

use serde::{Deserialize, Serialize};
use simd_json::owned::Value as JsonValue;
use std::collections::HashMap;

pub fn deep_merge_json(target: &mut JsonValue, source: JsonValue) {
    match (target, source) {
        (JsonValue::Object(target), JsonValue::Object(source)) => {
            for (key, value) in *source {
                match target.get_mut(key.as_str()) {
                    Some(existing) => deep_merge_json(existing, value),
                    None => {
                        target.insert(key, value);
                    }
                }
            }
        }
        (target, source) => *target = source,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenApiSpec {
    pub openapi: String,
    pub info: OpenApiInfo,
    pub paths: HashMap<String, PathItem>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub servers: Option<Vec<JsonValue>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<JsonValue>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhooks: Option<HashMap<String, PathItem>>,

    #[serde(skip_serializing_if = "Option::is_none", rename = "externalDocs")]
    pub external_docs: Option<JsonValue>,
    pub components: Option<Components>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenApiInfo {
    pub title: String,
    pub version: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none", rename = "termsOfService")]
    pub terms_of_service: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact: Option<JsonValue>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<JsonValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub get: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub put: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delete: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Vec<Parameter>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Vec<Parameter>>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "operationId")]
    pub operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<RequestBody>,
    pub responses: HashMap<String, Response>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callbacks: Option<HashMap<String, JsonValue>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security: Option<Vec<JsonValue>>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub examples: Option<JsonValue>,
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

    #[serde(skip_serializing_if = "HashMap::is_empty", rename = "securitySchemes")]
    pub security_schemes: HashMap<String, JsonValue>,
}

impl Default for OpenApiSpec {
    fn default() -> Self {
        Self {
            openapi: "3.1.0".to_string(),
            info: OpenApiInfo {
                title: "FastrAPI".to_string(),
                version: "0.1.0".to_string(),
                summary: None,
                description: Some("API built with FastrAPI".to_string()),
                terms_of_service: None,
                contact: None,
                license: None,
            },
            paths: HashMap::new(),
            servers: None,
            tags: None,
            webhooks: None,
            external_docs: None,
            components: Some(Components {
                schemas: HashMap::new(),
                security_schemes: HashMap::new(),
            }),
        }
    }
}
