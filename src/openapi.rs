use crate::utils::py_dict_to_json;
use papaya::HashMap as PapayaMap;
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

    if crate::pydantic::is_pydantic_model(py, type_hint) {
        let schema_name = get_schema_name(type_hint);
        return json!({"$ref": format!("#/components/schemas/{}", schema_name)});
    }

    json!({"type": "string"})
}

fn extract_param_info(
    py: Python,
    param_obj: &Bound<PyAny>,
) -> (Option<String>, Option<JsonValue>, bool) {
    let mut description = None;
    let mut schema = None;
    let mut required = true;

    if let Ok(default) = param_obj.getattr("default") {
        if let Ok(type_name) = default.get_type().name() {
            let type_str = type_name.to_string();

            if ["Query", "Path", "Header", "Cookie", "Body", "Form", "File"]
                .contains(&type_str.as_str())
            {
                if let Ok(desc) = default.getattr("description") {
                    if !desc.is_none() {
                        description = desc.extract::<String>().ok();
                    }
                }

                if let Ok(def_val) = default.getattr("default") {
                    required = def_val.is_none();
                }
            } else if !default.is_none() {
                required = false;
            }
        }
    }

    // type annotation
    if let Ok(annotation) = param_obj.getattr("annotation") {
        if !annotation.is_none() {
            schema = Some(python_type_to_openapi_type(py, &annotation));
        }
    }

    (description, schema, required)
}

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
            parameters: None,
            request_body: None,
            responses: HashMap::new(),
        };

        let mut parameters = Vec::new();

        if let Ok(inspect) = py.import("inspect") {
            if let Ok(sig) = inspect.call_method1("signature", (handler.func.bind(py),)) {
                if let Ok(params) = sig.getattr("parameters") {
                    if let Ok(params_dict) = params.cast::<PyDict>() {
                        for (param_name_obj, param_obj) in params_dict.iter() {
                            let param_name = param_name_obj.extract::<String>().unwrap_or_default();

                            if param_name == "self" || param_name == "cls" || param_name == "return"
                            {
                                continue;
                            }

                            let (description, schema, required) =
                                extract_param_info(py, &param_obj);

                            let location = if handler.path_param_names.contains(&param_name) {
                                "path"
                            } else if handler.query_param_names.contains(&param_name) {
                                "query"
                            } else if let Ok(default) = param_obj.getattr("default") {
                                if let Ok(type_name) = default.get_type().name() {
                                    let type_str = type_name.to_string();
                                    match type_str.as_str() {
                                        "Header" => "header",
                                        "Cookie" => "cookie",
                                        _ => continue,
                                    }
                                } else {
                                    continue;
                                }
                            } else {
                                continue;
                            };

                            parameters.push(Parameter {
                                name: param_name,
                                location: location.to_string(),
                                required: Some(required || location == "path"),
                                schema: schema.or(Some(json!({"type": "string"}))),
                                description,
                            });
                        }
                    }
                }
            }
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
                let mut request_schemas = HashMap::new();
                for (param_name, validator) in &handler.param_validators {
                    let validator_bound = validator.bind(py);

                    if let Some(schema) = extract_pydantic_schema(py, validator_bound) {
                        let schema_name = get_schema_name(validator_bound);
                        schemas.insert(schema_name.clone(), schema.clone());
                        request_schemas.insert(
                            param_name.clone(),
                            json!({
                                "$ref": format!("#/components/schemas/{}", schema_name)
                            }),
                        );
                    }
                }

                if !request_schemas.is_empty() {
                    operation.request_body = Some(RequestBody {
                        required: true,
                        content: {
                            let mut content = HashMap::new();
                            content.insert(
                                "application/json".to_string(),
                                MediaType {
                                    schema: json!({
                                        "type": "object",
                                        "properties": request_schemas,
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
        if !handler.param_validators.is_empty() || operation.parameters.is_some() {
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
    spec
}
