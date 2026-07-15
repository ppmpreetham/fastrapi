use crate::FastrAPI;
use crate::decorators::PyAPIRouter;
use crate::ffi::pydantic;
use crate::routing::types::{FlatRoute, ParameterConstraints, ParameterSource};
use crate::utils::py_dict_to_json;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};
use serde::{Deserialize, Serialize};
use sonic_rs::{JsonContainerTrait, JsonValueMutTrait, JsonValueTrait, Value as JsonValue, json};
use std::collections::HashMap;
use tracing::debug;

pub fn deep_merge_json(target: &mut JsonValue, source: JsonValue) {
    if source.is_object() && target.is_object() {
        let source_obj = source.as_object().unwrap();
        let target_obj = target.as_object_mut().unwrap();

        for (k, v) in source_obj.iter() {
            if target_obj.contains_key(&k) {
                let target_val = target_obj.get_mut(&k).unwrap();
                deep_merge_json(target_val, v.clone());
            } else {
                target_obj.insert(k, v.clone());
            }
        }
    } else {
        *target = source;
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
            }),
        }
    }
}

pub fn extract_pydantic_schema(py: Python, model: &Bound<PyAny>) -> Option<JsonValue> {
    // Pydantic v2
    if let Ok(schema_method) = model.getattr("model_json_schema")
        && let Ok(result) = schema_method.call0()
        && let Ok(dict) = result.cast::<PyDict>()
    {
        return Some(py_dict_to_json(py, dict));
    }

    // Pydantic v1
    if let Ok(schema_method) = model.getattr("schema")
        && let Ok(result) = schema_method.call0()
        && let Ok(dict) = result.cast::<PyDict>()
    {
        return Some(py_dict_to_json(py, dict));
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

fn python_type_to_openapi_type(py: Python<'_>, type_hint: &Bound<PyAny>) -> JsonValue {
    if pydantic::is_pydantic_model(py, type_hint) {
        let schema_name = get_schema_name(type_hint);
        return json!({
            "$ref": format!("#/components/schemas/{schema_name}")
        });
    }

    if let Ok(name_attr) = type_hint.getattr("__name__")
        && let Ok(py_str) = name_attr.cast::<PyString>()
        && let Ok(name_str) = py_str.to_str()
    {
        match name_str {
            "str" => return json!({ "type": "string" }),
            "int" => return json!({ "type": "integer" }),
            "float" => return json!({ "type": "number" }),
            "bool" => return json!({ "type": "boolean" }),
            "list" => {
                return json!({
                    "type": "array",
                    "items": { "type": "string" }
                });
            }
            "dict" => return json!({ "type": "object" }),
            _ => {}
        }
    }

    if let Ok(type_repr) = type_hint.str()
        && let Ok(type_name) = type_repr.to_str()
    {
        if type_name.contains("List") || type_name.contains("list") {
            return json!({
                "type": "array",
                "items": { "type": "string" }
            });
        }

        if type_name.contains("Dict") || type_name.contains("dict") {
            return json!({
                "type": "object"
            });
        }
    }

    json!({ "type": "string" })
}

fn apply_parameter_constraints(
    mut schema: JsonValue,
    constraints: &ParameterConstraints,
) -> JsonValue {
    if let Some(object) = schema.as_object_mut() {
        if let Some(gt) = constraints.gt {
            object.insert("exclusiveMinimum", json!(gt));
        }
        if let Some(ge) = constraints.ge {
            object.insert("minimum", json!(ge));
        }
        if let Some(lt) = constraints.lt {
            object.insert("exclusiveMaximum", json!(lt));
        }
        if let Some(le) = constraints.le {
            object.insert("maximum", json!(le));
        }
        if let Some(min_length) = constraints.min_length {
            object.insert("minLength", json!(min_length));
        }
        if let Some(max_length) = constraints.max_length {
            object.insert("maxLength", json!(max_length));
        }

        if let Some(pattern) = &constraints.pattern {
            object.insert("pattern", json!(pattern.as_str()));
        }
    }
    schema
}

pub fn build_openapi_spec(py: Python<'_>, app: &FastrAPI) -> JsonValue {
    let mut spec = OpenApiSpec::default();

    spec.info.title = app.title.clone();
    spec.info.version = app.version.clone();
    spec.info.summary = app.summary.clone();

    spec.info.description = (!app.description.is_empty()).then(|| app.description.clone());
    spec.info.terms_of_service = app.terms_of_service.clone();

    if let Some(contact) = &app.contact
        && let Ok(dict) = contact.bind(py).cast::<PyDict>()
    {
        spec.info.contact = Some(py_dict_to_json(py, dict));
    }

    if let Some(license) = &app.license_info
        && let Ok(dict) = license.bind(py).cast::<PyDict>()
    {
        spec.info.license = Some(py_dict_to_json(py, dict));
    }

    if let Some(servers) = &app.servers
        && let Ok(list) = servers.extract::<Vec<Py<PyAny>>>(py)
    {
        spec.servers = Some(
            list.into_iter()
                .filter_map(|item| {
                    item.bind(py)
                        .cast::<PyDict>()
                        .ok()
                        .map(|d| py_dict_to_json(py, d))
                })
                .collect(),
        );
    }

    if app.root_path_in_servers && !app.root_path.is_empty() {
        spec.servers.get_or_insert_with(Vec::new).push(json!({
            "url": app.root_path
        }));
    }

    if let Some(tags) = &app.openapi_tags
        && let Ok(list) = tags.extract::<Vec<Py<PyAny>>>(py)
    {
        spec.tags = Some(
            list.into_iter()
                .filter_map(|item| {
                    item.bind(py)
                        .cast::<PyDict>()
                        .ok()
                        .map(|d| py_dict_to_json(py, d))
                })
                .collect(),
        );
    }

    if let Some(docs) = &app.openapi_external_docs
        && let Ok(dict) = docs.bind(py).cast::<PyDict>()
    {
        spec.external_docs = Some(py_dict_to_json(py, dict));
    }

    let router = app.router.bind(py);
    let router = router.borrow();
    let collected = collect_routes(py, &router);

    let app_responses = if let Some(resp) = &app.responses
        && let Ok(dict) = resp.bind(py).cast::<PyDict>()
    {
        Some(py_dict_to_json(py, dict))
    } else {
        None
    };

    let mut schemas: HashMap<String, JsonValue> = HashMap::new();
    spec.paths = build_paths_from_routes(py, collected, &mut schemas, app_responses.as_ref());

    if let Some(wh) = &app.webhooks
        && let Ok(router) = wh.bind(py).cast::<crate::decorators::PyAPIRouter>()
    {
        let wh_collected = collect_routes(py, &router.borrow());
        let wh_paths = build_paths_from_routes(py, wh_collected, &mut schemas, None);
        spec.webhooks = Some(wh_paths);
    }

    if let Some(components) = &mut spec.components {
        components.schemas = schemas;
    }
    debug!("Built OpenAPI spec with {} paths", spec.paths.len());
    sonic_rs::to_value(&spec).unwrap_or_else(|_| json!({}))
}

pub fn build_paths_from_routes(
    py: Python<'_>,
    collected: Vec<FlatRoute>,
    schemas: &mut HashMap<String, JsonValue>,
    app_responses: Option<&JsonValue>,
) -> HashMap<String, PathItem> {
    let mut paths: HashMap<String, PathItem> = HashMap::new();

    for route in collected {
        if !route.include_in_schema {
            continue;
        }

        let path = route.path.clone();
        let method = route.method.as_str().to_lowercase();
        let handler = route.handler.clone();
        let tags = &route.tags;

        let description = handler
            .func
            .bind(py)
            .getattr("__doc__")
            .ok()
            .and_then(|doc| doc.extract::<String>().ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let mut operation = Operation {
            summary: route.summary.clone(),
            description: route.description.clone().or(description),
            operation_id: route.operation_id.clone(),
            tags: if tags.is_empty() {
                None
            } else {
                Some(tags.clone())
            },
            deprecated: route.deprecated,
            parameters: None,
            request_body: None,
            responses: HashMap::new(),
            callbacks: None,
        };

        let mut parameters = Vec::new();

        handler.parsed_params.iter().for_each(|param| {
            let location = match param.source {
                ParameterSource::Path => "path",
                ParameterSource::Query => "query",
                ParameterSource::Header => "header",
                ParameterSource::Cookie => "cookie",
                ParameterSource::Body | ParameterSource::BackgroundTasks => return,
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
        });

        if !parameters.is_empty() {
            operation.parameters = Some(parameters);
        }

        // Request body for POST/PUT/PATCH with validators
        if !handler.param_validators.is_empty()
            && ["post", "put", "patch"].contains(&method.as_str())
        {
            let validator_count = handler.param_validators.len();

            if validator_count == 1 {
                let validator = &handler.param_validators[0];
                let validator_bound = validator.model_class.bind(py);

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
                let mut properties = sonic_rs::Object::new();
                let mut required_fields = Vec::new();

                handler.param_validators.iter().for_each(|validator| {
                    let validator_bound = validator.model_class.bind(py);

                    if let Some(schema) = extract_pydantic_schema(py, validator_bound) {
                        let schema_name = get_schema_name(validator_bound);
                        schemas.insert(schema_name.clone(), schema);
                        properties.insert(
                            &validator.name,
                            json!({ "$ref": format!("#/components/schemas/{}", schema_name) }),
                        );
                        required_fields.push(validator.name.clone());
                    }
                });

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
                        .replace(['{', '}'], "_");

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
        let response_desc = route
            .response_description
            .clone()
            .unwrap_or_else(|| "Successful Response".to_string());
        // Default 200 response
        operation.responses.insert(
            "200".to_string(),
            Response {
                description: response_desc,
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

        let path_item = paths.entry(path).or_insert_with(|| PathItem {
            get: None,
            post: None,
            put: None,
            delete: None,
            patch: None,
            parameters: None,
        });

        let mut operation_val = sonic_rs::to_value(&operation).unwrap_or_else(|_| json!({}));

        if let Some(extra) = &route.openapi_extra {
            deep_merge_json(&mut operation_val, extra.clone());
        }

        // Global app.responses merge
        if let Some(app_resps) = app_responses {
            if let Some(op_obj) = operation_val.as_object_mut() {
                if let Some(op_resp) = op_obj.get_mut(&"responses") {
                    deep_merge_json(op_resp, app_resps.clone());
                }
            }
        }

        // Route responses override app.responses
        if let Some(responses) = &route.responses {
            if let Some(op_obj) = operation_val.as_object_mut() {
                if let Some(op_resp) = op_obj.get_mut(&"responses") {
                    deep_merge_json(op_resp, responses.clone());
                }
            }
        }

        // Callbacks handling
        if let Some(callbacks_val) = &route.callbacks {
            if let Some(op_obj) = operation_val.as_object_mut() {
                if let Some(op_cb) = op_obj.get_mut(&"callbacks") {
                    deep_merge_json(op_cb, callbacks_val.clone());
                } else {
                    op_obj.insert("callbacks", callbacks_val.clone());
                }
            }
        }

        match method.as_str() {
            "get" => path_item.get = Some(operation_val),
            "post" => path_item.post = Some(operation_val),
            "put" => path_item.put = Some(operation_val),
            "delete" => path_item.delete = Some(operation_val),
            "patch" => path_item.patch = Some(operation_val),
            _ => {}
        }
    }
    paths
}

pub fn parse_callbacks_to_json(
    py: Python<'_>,
    callbacks_bound: &Bound<'_, PyAny>,
) -> Option<JsonValue> {
    let mut callbacks_map = sonic_rs::Object::new();
    let mut dummy_schemas = HashMap::new();

    if let Ok(list) = callbacks_bound.try_iter() {
        for item in list.flatten() {
            if let Ok(router_ref) = item.cast::<crate::decorators::PyAPIRouter>() {
                let router = router_ref.borrow();
                let collected = collect_routes(py, &router);
                let paths = build_paths_from_routes(py, collected, &mut dummy_schemas, None);

                for (path, path_item) in paths {
                    callbacks_map.insert(
                        &path,
                        sonic_rs::to_value(&path_item).unwrap_or_else(|_| json!({})),
                    );
                }
            }
        }
    } else if let Ok(dict) = callbacks_bound.cast::<PyDict>() {
        for (k, v) in dict {
            if let Ok(k_str) = k.extract::<String>() {
                if let Ok(list) = v.try_iter() {
                    let mut inner_map = sonic_rs::Object::new();
                    for item in list.flatten() {
                        if let Ok(router_ref) = item.cast::<crate::decorators::PyAPIRouter>() {
                            let router = router_ref.borrow();
                            let collected = collect_routes(py, &router);
                            let paths =
                                build_paths_from_routes(py, collected, &mut dummy_schemas, None);
                            for (path, path_item) in paths {
                                inner_map.insert(
                                    &path,
                                    sonic_rs::to_value(&path_item).unwrap_or_else(|_| json!({})),
                                );
                            }
                        }
                    }
                    callbacks_map.insert(&k_str, json!(inner_map));
                }
            }
        }
    }

    if callbacks_map.is_empty() {
        None
    } else {
        Some(json!(callbacks_map))
    }
}

fn collect_routes(py: Python<'_>, router: &PyAPIRouter) -> Vec<FlatRoute> {
    let flat = router.flatten(py);
    flat.0.clone()
}
