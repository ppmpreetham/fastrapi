use super::document::{
    MediaType, Operation, Parameter, PathItem, RequestBody, Response, deep_merge_json,
};
use super::schema::{
    apply_parameter_constraints, body_model_name, extract_pydantic_schema, get_schema_name,
    native_operation_id, python_type_to_openapi_type, schema_title,
};
use super::spec::RouteCollector;
use fastrapi_core::method::HttpMethod;
use fastrapi_core::params::ParameterSource;
use fastrapi_core::path::strip_path_convertors;
use fastrapi_core::security::SecurityKind;
use fastrapi_pyutil::json::py_any_to_json;
use fastrapi_routing::route::{RouteEntry, RouteHandler};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use simd_json::json;
use simd_json::owned::{Object, Value as JsonValue};
use simd_json::prelude::{ValueAsMutObject, ValueAsObject, ValueObjectAccess};
use simd_json::serde::to_owned_value;
use std::collections::HashMap;

/// `OpenAPI` securitySchemes entry
fn scheme_to_openapi(
    kind: &SecurityKind,
    description: Option<&str>,
    scopes: Option<&simd_json::OwnedValue>,
) -> JsonValue {
    use fastrapi_core::security::SecurityKind;

    let mut base = match kind {
        SecurityKind::OAuth2PasswordBearer { token_url, .. } => {
            let flow_scopes = scopes.cloned().unwrap_or_else(|| json!({}));
            json!({
                "type": "oauth2",
                "flows": { "password": { "tokenUrl": token_url, "scopes": flow_scopes } }
            })
        }
        SecurityKind::OAuth2AuthorizationCode {
            authorization_url,
            token_url,
            refresh_url,
            ..
        } => {
            let flow_scopes = scopes.cloned().unwrap_or_else(|| json!({}));
            let mut code_flow = json!({
                "authorizationUrl": authorization_url,
                "tokenUrl": token_url,
                "scopes": flow_scopes,
            });
            if let Some(refresh) = refresh_url
                && let Some(obj) = code_flow.as_object_mut()
            {
                let key = String::from("refreshUrl");
                obj.insert(key, json!(refresh));
            }
            json!({ "type": "oauth2", "flows": { "authorizationCode": code_flow } })
        }
        SecurityKind::OpenIdConnect { url, .. } => json!({
            "type": "openIdConnect",
            "openIdConnectUrl": url
        }),
        SecurityKind::HTTPBearer { bearer_format, .. } => {
            let mut entry = json!({ "type": "http", "scheme": "bearer" });
            if let Some(fmt) = bearer_format
                && let Some(obj) = entry.as_object_mut()
            {
                let key = String::from("bearerFormat");
                obj.insert(key, json!(fmt));
            }
            entry
        }
        SecurityKind::HTTPBasic { .. } => json!({ "type": "http", "scheme": "basic" }),
        SecurityKind::HTTPDigest { .. } => json!({ "type": "http", "scheme": "digest" }),
        SecurityKind::APIKeyHeader { name, .. } => {
            json!({ "type": "apiKey", "in": "header", "name": name })
        }
        SecurityKind::APIKeyQuery { name, .. } => {
            json!({ "type": "apiKey", "in": "query", "name": name })
        }
        SecurityKind::APIKeyCookie { name, .. } => {
            json!({ "type": "apiKey", "in": "cookie", "name": name })
        }
    };

    if let (Some(obj), Some(desc)) = (base.as_object_mut(), description) {
        let key = String::from("description");
        obj.insert(key, json!(desc));
    }

    base
}

fn build_request_body(
    py: Python<'_>,
    handler: &RouteHandler,
    route: &RouteEntry,
    schemas: &mut HashMap<String, JsonValue>,
) -> Option<RequestBody> {
    if handler.validation.param_validators.is_empty()
        || !matches!(
            route.method,
            HttpMethod::POST | HttpMethod::PUT | HttpMethod::PATCH
        )
    {
        return None;
    }

    let validators = &handler.validation.param_validators;
    if validators.len() == 1 {
        let validator_bound = validators[0].model_class.bind(py);
        let schema = extract_pydantic_schema(py, validator_bound, "validation")?;
        let schema_name = get_schema_name(validator_bound);
        schemas.insert(schema_name.clone(), schema);

        // fastapi `Body(embed=True)`: the body is an object with the model under
        // the parameter name, described by a synthetic `Body_*` schema
        let embedded = handler
            .payload
            .parsed_params
            .iter()
            .find(|param| param.validator_index == Some(0))
            .is_some_and(|param| param.embed);

        let body_schema = if embedded {
            let param_name = handler
                .payload
                .parsed_params
                .iter()
                .find(|param| param.validator_index == Some(0))
                .map_or_else(
                    || validators[0].name.clone(),
                    |param| param.external_name.clone(),
                );
            let wrapper_name = body_model_name(
                handler.execution.func.bind(py),
                &strip_path_convertors(&route.path),
                route.method.as_ref(),
            );
            schemas.insert(
                wrapper_name.clone(),
                json!({
                    "properties": { param_name.clone(): { "$ref": format!("#/components/schemas/{schema_name}") } },
                    "type": "object",
                    "required": [param_name],
                    "title": wrapper_name,
                }),
            );
            json!({ "$ref": format!("#/components/schemas/{wrapper_name}") })
        } else {
            json!({ "$ref": format!("#/components/schemas/{}", schema_name) })
        };

        let mut content = HashMap::new();
        content.insert(
            "application/json".to_string(),
            MediaType {
                schema: body_schema,
            },
        );
        return Some(RequestBody {
            required: true,
            content,
        });
    }

    let mut properties = Object::new();
    let mut required_fields = Vec::new();

    for validator in validators {
        let validator_bound = validator.model_class.bind(py);
        let Some(schema) = extract_pydantic_schema(py, validator_bound, "validation") else {
            continue;
        };
        let schema_name = get_schema_name(validator_bound);
        schemas.insert(schema_name.clone(), schema);
        properties.insert(
            validator.name.clone(),
            json!({ "$ref": format!("#/components/schemas/{}", schema_name) }),
        );
        required_fields.push(validator.name.clone());
    }

    if properties.is_empty() {
        return None;
    }

    let wrapper_name = body_model_name(
        handler.execution.func.bind(py),
        &strip_path_convertors(&route.path),
        route.method.as_ref(),
    );

    schemas.insert(
        wrapper_name.clone(),
        json!({
            "type": "object",
            "properties": properties,
            "required": required_fields,
        }),
    );

    let mut content = HashMap::new();
    content.insert(
        "application/json".to_string(),
        MediaType {
            schema: json!({ "$ref": format!("#/components/schemas/{}", wrapper_name) }),
        },
    );
    Some(RequestBody {
        required: true,
        content,
    })
}

pub fn build_paths_from_routes(
    py: Python<'_>,
    collected: &[RouteEntry],
    schemas: &mut HashMap<String, JsonValue>,
    app_responses: Option<&JsonValue>,
    separate_input_output_schemas: bool,
    generate_unique_id_function: Option<&Py<PyAny>>,
    security_schemes: &mut HashMap<String, JsonValue>,
) -> HashMap<String, PathItem> {
    let mut paths: HashMap<String, PathItem> = HashMap::new();

    for route in collected {
        if !route.include_in_schema {
            continue;
        }

        // documented paths never carry starlette convertors: `{p:int}` is `{p}`
        let path = strip_path_convertors(&route.path);

        let handler = route.handler.clone();
        let tags = &route.tags;

        let description = handler
            .execution
            .func
            .bind(py)
            .getattr("__doc__")
            .ok()
            .and_then(|doc| doc.extract::<String>().ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // fastapi defaults the summary to the endpoint name as `{name}{path}_{method}`.
        let func_name = handler
            .execution
            .func
            .bind(py)
            .getattr(intern!(py, "__name__"))
            .ok()
            .and_then(|name| name.extract::<String>().ok())
            .unwrap_or_else(|| "endpoint".to_string());
        let summary = route
            .summary
            .clone()
            .or_else(|| Some(schema_title(&func_name)));
        let operation_id = route.operation_id.clone().or_else(|| {
            generate_unique_id_function.and_then(|f| {
                f.bind(py)
                    .call1((handler.execution.func.bind(py),))
                    .ok()
                    .and_then(|res| res.extract::<String>().ok())
            })
        });

        let mut operation = Operation {
            summary,
            description: route.description.clone().or(description),
            operation_id: operation_id.or_else(|| {
                // fastapi's unique id uses the path without convertor suffixes
                Some(native_operation_id(
                    &func_name,
                    &strip_path_convertors(&route.path),
                    route.method.as_ref(),
                ))
            }),
            tags: (!tags.is_empty()).then(|| tags.clone()),
            deprecated: route.deprecated,
            parameters: None,
            request_body: None,
            responses: HashMap::new(),
            callbacks: None,
            security: None,
        };

        if !route.security.is_empty() {
            let mut grouped = Object::new();
            for req in &route.security {
                let compiled = req.scheme.as_ref();
                security_schemes
                    .entry(compiled.name.clone())
                    .or_insert_with(|| {
                        scheme_to_openapi(
                            &compiled.kind,
                            compiled.description.as_deref(),
                            compiled.scopes.as_ref(),
                        )
                    });
                let scope_list: Vec<JsonValue> = req.scopes.iter().map(|s| json!(s)).collect();
                grouped.insert(compiled.name.clone(), json!(scope_list));
            }
            operation.security = Some(vec![to_owned_value(&grouped).unwrap_or_else(|_| json!({}))]);
        }

        let parameters: Vec<Parameter> = handler
            .payload
            .parsed_params
            .iter()
            .filter_map(|param| {
                let location = match param.source {
                    ParameterSource::Path => "path",
                    ParameterSource::Query => "query",
                    ParameterSource::Header => "header",
                    ParameterSource::Cookie => "cookie",
                    ParameterSource::Body | ParameterSource::BackgroundTasks => return None,
                };

                // the `Query()`/`Path()`/... marker instance carries the fastapi
                // per-parameter metadata; plain params have no marker and default.
                let marker = param.param_object.as_ref().map(|marker| marker.bind(py));

                let include_in_schema = marker
                    .and_then(|marker| marker.getattr(intern!(py, "include_in_schema")).ok())
                    .and_then(|value| value.extract::<bool>().ok())
                    .unwrap_or(true);
                if !include_in_schema {
                    return None;
                }

                let mut schema = param.annotation.as_ref().map_or_else(
                    || json!({"type": "string"}),
                    |annotation| python_type_to_openapi_type(py, annotation.bind(py), schemas),
                );

                // fastapi puts the `examples=[...]` list on the parameter's json schema
                if let Some(examples) = marker
                    .and_then(|marker| marker.getattr(intern!(py, "examples")).ok())
                    .filter(|value| !value.is_none())
                    && let Some(obj) = schema.as_object_mut()
                {
                    obj.insert("examples".to_string(), py_any_to_json(py, &examples));
                }

                // pydantic derives a `title` from the parameter name and keeps
                // explicit non-`None` defaults visible in the schema; `$ref`
                // schemas (pydantic models, enums) stay untouched.
                let is_ref = schema
                    .as_object()
                    .is_some_and(|obj| obj.contains_key("$ref"));
                if !is_ref && let Some(obj) = schema.as_object_mut() {
                    if !obj.contains_key("title") {
                        obj.insert(
                            "title".to_string(),
                            json!(schema_title(&param.external_name)),
                        );
                    }
                    if param.has_default
                        && let Some(default) = &param.default_value
                        && !default.bind(py).is_none()
                    {
                        obj.insert("default".to_string(), py_any_to_json(py, default.bind(py)));
                    }
                }

                let schema = apply_parameter_constraints(schema, &param.constraints);

                let deprecated = marker
                    .and_then(|marker| marker.getattr(intern!(py, "deprecated")).ok())
                    .filter(|value| !value.is_none())
                    .and_then(|value| value.is_truthy().ok())
                    .filter(|truthy| *truthy)
                    .map(|_| true);
                let example = marker
                    .and_then(|marker| marker.getattr(intern!(py, "example")).ok())
                    .filter(|value| !value.is_none())
                    .map(|value| py_any_to_json(py, &value));
                let openapi_examples = marker
                    .and_then(|marker| marker.getattr(intern!(py, "openapi_examples")).ok())
                    .filter(|value| !value.is_none())
                    .map(|value| py_any_to_json(py, &value));

                Some(Parameter {
                    name: param.external_name.clone(),
                    location: location.to_string(),
                    required: Some(param.required || location == "path"),
                    schema: Some(schema),
                    description: param.description.clone(),
                    deprecated,
                    example,
                    examples: openapi_examples,
                })
            })
            .collect();

        operation.parameters = (!parameters.is_empty()).then_some(parameters);

        operation.request_body = build_request_body(py, &handler, route, schemas);
        let response_desc = route
            .response_description
            .clone()
            .unwrap_or_else(|| "Successful Response".to_string());

        let mut response_schema = json!({});
        if let Some(rm) = &handler.response.response_model {
            let rm_bound = rm.bind(py);
            let mode = if separate_input_output_schemas {
                "serialization"
            } else {
                "validation"
            };
            if let Some(schema) = extract_pydantic_schema(py, rm_bound, mode) {
                let schema_name = get_schema_name(rm_bound);
                if schema_name.starts_with("list[")
                    || schema_name.starts_with("tuple[")
                    || schema_name.starts_with("typing.")
                {
                    if let Some(defs) = schema.get("$defs").and_then(JsonValue::as_object) {
                        schemas.extend(
                            defs.iter()
                                .map(|(name, value)| (name.clone(), value.clone())),
                        );
                    }
                    if let Some(defs) = schema.get("definitions").and_then(JsonValue::as_object) {
                        schemas.extend(
                            defs.iter()
                                .map(|(name, value)| (name.clone(), value.clone())),
                        );
                    }
                    response_schema = schema;
                } else {
                    schemas.insert(schema_name.clone(), schema);
                    response_schema =
                        json!({ "$ref": format!("#/components/schemas/{}", schema_name) });
                }
            }
        }

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
                            schema: response_schema,
                        },
                    );
                    content
                }),
            },
        );

        // 422 for validation errors, documented with the shared component schemas
        if !handler.validation.param_validators.is_empty()
            || !handler.payload.parsed_params.is_empty()
        {
            schemas
                .entry("HTTPValidationError".to_string())
                .or_insert_with(|| {
                    json!({
                        "properties": {
                            "detail": {
                                "items": { "$ref": "#/components/schemas/ValidationError" },
                                "type": "array",
                                "title": "Detail"
                            }
                        },
                        "type": "object",
                        "title": "HTTPValidationError"
                    })
                });
            schemas.entry("ValidationError".to_string()).or_insert_with(|| {
                json!({
                    "properties": {
                        "loc": {
                            "items": { "anyOf": [ { "type": "string" }, { "type": "integer" } ] },
                            "type": "array",
                            "title": "Location"
                        },
                        "msg": { "type": "string", "title": "Message" },
                        "type": { "type": "string", "title": "Error Type" },
                        "input": { "title": "Input" },
                        "ctx": { "type": "object", "title": "Context" }
                    },
                    "type": "object",
                    "required": ["loc", "msg", "type"],
                    "title": "ValidationError"
                })
            });
            operation.responses.insert(
                "422".to_string(),
                Response {
                    description: "Validation Error".to_string(),
                    content: Some({
                        let mut content = HashMap::new();
                        content.insert(
                            "application/json".to_string(),
                            MediaType {
                                schema: json!({ "$ref": "#/components/schemas/HTTPValidationError" }),
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

        let mut operation_val = to_owned_value(&operation).unwrap_or_else(|_| json!({}));

        if let Some(extra) = &route.openapi_extra {
            deep_merge_json(&mut operation_val, extra.clone());
        }

        // Global app.responses merge
        if let Some(app_resps) = app_responses
            && let Some(op_obj) = operation_val.as_object_mut()
            && let Some(op_resp) = op_obj.get_mut("responses")
        {
            deep_merge_json(op_resp, app_resps.clone());
        }

        // Route responses override app.responses
        if let Some(responses) = &route.responses
            && let Some(op_obj) = operation_val.as_object_mut()
            && let Some(op_resp) = op_obj.get_mut("responses")
        {
            deep_merge_json(op_resp, responses.clone());
        }

        // Callbacks handling
        if let Some(callbacks_val) = &route.callbacks
            && let Some(op_obj) = operation_val.as_object_mut()
        {
            if let Some(op_cb) = op_obj.get_mut("callbacks") {
                deep_merge_json(op_cb, callbacks_val.clone());
            } else {
                op_obj.insert("callbacks".to_string(), callbacks_val.clone());
            }
        }

        match route.method {
            HttpMethod::GET => path_item.get = Some(operation_val),
            HttpMethod::POST => path_item.post = Some(operation_val),
            HttpMethod::PUT => path_item.put = Some(operation_val),
            HttpMethod::DELETE => path_item.delete = Some(operation_val),
            HttpMethod::PATCH => path_item.patch = Some(operation_val),
            _ => {}
        }
    }
    paths
}

#[must_use]
pub fn parse_callbacks_to_json(
    py: Python<'_>,
    callbacks_bound: &Bound<'_, PyAny>,
    collect: RouteCollector,
) -> Option<JsonValue> {
    let mut callbacks_map = Object::new();
    let mut dummy_schemas = HashMap::new();
    let mut dummy_security = HashMap::new();

    if let Ok(list) = callbacks_bound.try_iter() {
        for item in list.flatten() {
            if let Some(collected) = collect(py, &item) {
                let paths = build_paths_from_routes(
                    py,
                    &collected,
                    &mut dummy_schemas,
                    None,
                    false,
                    None,
                    &mut dummy_security,
                );

                for (path, path_item) in paths {
                    let value = to_owned_value(&path_item).unwrap_or_else(|_| json!({}));
                    callbacks_map.insert(path, value);
                }
            }
        }
    } else if let Ok(dict) = callbacks_bound.cast::<PyDict>() {
        for (k, v) in dict.iter() {
            if let Ok(k_str) = k.extract::<String>()
                && let Ok(list) = v.try_iter()
            {
                let mut inner_map = Object::new();
                for item in list.flatten() {
                    if let Some(collected) = collect(py, &item) {
                        let paths = build_paths_from_routes(
                            py,
                            &collected,
                            &mut dummy_schemas,
                            None,
                            false,
                            None,
                            &mut dummy_security,
                        );

                        for (path, path_item) in paths {
                            let value = to_owned_value(&path_item).unwrap_or_else(|_| json!({}));
                            inner_map.insert(path, value);
                        }
                    }
                }
                callbacks_map.insert(k_str, json!(inner_map));
            }
        }
    }

    if callbacks_map.is_empty() {
        None
    } else {
        Some(JsonValue::Object(Box::new(callbacks_map)))
    }
}
