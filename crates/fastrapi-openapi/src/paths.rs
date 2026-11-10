//! Builds the OpenAPI paths object from the routes.
//! It makes the operations, parameters, request bodies, responses, callbacks, and security schemes for each route.

use super::document::{
    MediaType, Operation, Parameter, PathItem, RequestBody, Response, deep_merge_json,
};
use super::registry::{SchemaRegistry, substitute};
use super::schema::{
    apply_parameter_constraints, native_operation_id, python_type_to_openapi_type, schema_title,
};
use super::spec::RouteCollector;
use fastrapi_core::method::HttpMethod;
use fastrapi_core::params::ParameterSource;
use fastrapi_core::path::strip_path_convertors;
use fastrapi_core::security::SecurityKind;
use fastrapi_http::response_type::ResponseType;
use fastrapi_pyutil::json::{json_to_py_object, py_any_to_json};
use fastrapi_routing::dependencies::InjectionType;
use fastrapi_routing::route::{ParsedParameter, RouteEntry, RouteHandler};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};
use simd_json::json;
use simd_json::owned::{Object, Value as JsonValue};
use simd_json::prelude::{ValueAsMutObject, ValueAsObject, ValueAsScalar};
use simd_json::serde::to_owned_value;
use std::collections::HashMap;

/// Makes the `OpenAPI` `securitySchemes` entry for a security scheme.
fn scheme_to_openapi(
    kind: &SecurityKind,
    description: Option<&str>,
    scopes: Option<&simd_json::OwnedValue>,
) -> JsonValue {
    use fastrapi_core::security::SecurityKind;

    let mut base = match kind {
        SecurityKind::OAuth2PasswordBearer {
            token_url,
            refresh_url,
            ..
        } => {
            let flow_scopes = scopes.cloned().unwrap_or_else(|| json!({}));
            let mut password_flow = json!({ "tokenUrl": token_url, "scopes": flow_scopes });
            if let Some(refresh) = refresh_url
                && let Some(obj) = password_flow.as_object_mut()
            {
                obj.insert(String::from("refreshUrl"), json!(refresh));
            }
            json!({ "type": "oauth2", "flows": { "password": password_flow } })
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
        SecurityKind::OAuth2 { flows, .. } => json!({ "type": "oauth2", "flows": flows }),
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

fn route_object<'py>(
    py: Python<'py>,
    route: &RouteEntry,
    handler: &RouteHandler,
) -> Option<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs
        .set_item("methods", vec![route.method.as_ref()])
        .ok()?;
    kwargs.set_item("name", &route.name).ok()?;
    kwargs.set_item("tags", &route.tags).ok()?;
    kwargs.set_item("summary", &route.summary).ok()?;
    kwargs.set_item("description", &route.description).ok()?;
    kwargs.set_item("operation_id", &route.operation_id).ok()?;
    kwargs.set_item("deprecated", route.deprecated).ok()?;
    kwargs
        .set_item("include_in_schema", route.include_in_schema)
        .ok()?;
    kwargs
        .set_item("path_format", strip_path_convertors(&route.path))
        .ok()?;
    py.import("fastrapi.routing")
        .ok()?
        .getattr("APIRoute")
        .ok()?
        .call(
            (route.path.as_str(), handler.execution.func.bind(py)),
            Some(&kwargs),
        )
        .ok()
}

pub fn apply_route_overrides(route: &Bound<'_, PyAny>, entry: &mut RouteEntry) {
    let text = |name: &str| {
        route
            .getattr(name)
            .ok()
            .and_then(|value| value.extract::<Option<String>>().ok())
    };
    if let Some(operation_id) = text("operation_id") {
        entry.operation_id = operation_id;
    }
    if let Some(summary) = text("summary") {
        entry.summary = summary;
    }
    if let Some(description) = text("description") {
        entry.description = description;
    }
    if let Some(Some(name)) = text("name") {
        entry.name = name;
    }
    if let Ok(path) = route
        .getattr("path")
        .and_then(|path| path.extract::<String>())
    {
        entry.path = path;
    }
    if let Ok(tags) = route
        .getattr("tags")
        .and_then(|tags| tags.extract::<Vec<String>>())
    {
        entry.tags = tags;
    }
    if let Ok(include) = route
        .getattr("include_in_schema")
        .and_then(|value| value.extract::<bool>())
    {
        entry.include_in_schema = include;
    }
    if let Ok(deprecated) = route
        .getattr("deprecated")
        .and_then(|value| value.extract::<Option<bool>>())
    {
        entry.deprecated = deprecated;
    }
}

fn status_text(py: Python<'_>, code: &str) -> Option<String> {
    let range = match code {
        "1XX" => Some("Information"),
        "2XX" => Some("Success"),
        "3XX" => Some("Redirection"),
        "4XX" => Some("Client Error"),
        "5XX" => Some("Server Error"),
        "DEFAULT" => Some("Default Response"),
        _ => None,
    };
    if let Some(range) = range {
        return Some(range.to_string());
    }
    let status = code.parse::<u16>().ok()?;
    py.import("http.client")
        .ok()?
        .getattr("responses")
        .ok()?
        .get_item(status)
        .ok()?
        .extract::<String>()
        .ok()
}

fn additional_responses(
    py: Python<'_>,
    responses: &Bound<'_, PyAny>,
    existing: &HashMap<String, Response>,
    unique_id: &str,
    registry: &mut SchemaRegistry,
) -> Option<JsonValue> {
    let dict = responses.cast::<PyDict>().ok()?;
    let mut merged = Object::new();
    for (code, response) in dict.iter() {
        let upper = code.str().ok()?.to_string().to_uppercase();
        let key = if upper == "DEFAULT" {
            "default".to_string()
        } else {
            upper.clone()
        };
        let mut processed = json!({});
        let mut model = None;
        if let Ok(response) = response.cast::<PyDict>() {
            for (name, value) in response.iter() {
                let name = name.extract::<String>().ok()?;
                if name == "model" {
                    model = Some(value).filter(|value| !value.is_none());
                } else if let Some(object) = processed.as_object_mut() {
                    object.insert(name, py_any_to_json(py, &value));
                }
            }
        }
        let object = processed.as_object_mut()?;
        if let Some(model) = model {
            let schema = registry.field(
                &model,
                "serialization",
                Some(schema_title(&format!("Response_{upper}_{unique_id}"))),
            );
            let content = object
                .entry("content".to_string())
                .or_insert_with(|| json!({}));
            if let Some(content) = content.as_object_mut() {
                let media = content
                    .entry("application/json".to_string())
                    .or_insert_with(|| json!({}));
                if let Some(media) = media.as_object_mut()
                    && !media.contains_key("schema")
                {
                    media.insert("schema".to_string(), schema);
                }
            }
        }
        let has_description = object
            .get("description")
            .is_some_and(|description| description.as_str().is_some_and(|text| !text.is_empty()));
        if !has_description {
            let description = existing
                .get(&key)
                .map(|response| response.description.clone())
                .or_else(|| status_text(py, &upper))
                .unwrap_or_else(|| "Additional Response".to_string());
            object.insert("description".to_string(), json!(description));
        }
        merged.insert(key, processed);
    }
    Some(JsonValue::Object(Box::new(merged)))
}

fn marker_class(py: Python<'_>, param: &ParsedParameter) -> Option<String> {
    let marker = param.param_object.as_ref()?.bind(py).get_type();
    Some(marker.name().ok()?.to_string())
}

fn body_field_schema(
    py: Python<'_>,
    param: &ParsedParameter,
    registry: &mut SchemaRegistry,
) -> JsonValue {
    let title = schema_title(&param.external_name);
    if param.is_file || marker_class(py, param).as_deref() == Some("File") {
        let binary = json!({ "type": "string", "contentMediaType": "application/octet-stream" });
        return if param.is_list {
            json!({ "type": "array", "items": binary, "title": title })
        } else {
            json!({ "type": "string", "contentMediaType": "application/octet-stream", "title": title })
        };
    }
    let Some(annotation) = param.annotation.as_ref() else {
        return json!({ "title": title.as_str() });
    };
    let annotation = match marker_attr(py, param, intern!(py, "discriminator"))
        .or_else(|| annotated_discriminator(py, param))
    {
        Some(discriminator) => discriminated(py, annotation.bind(py), &discriminator)
            .unwrap_or_else(|_| annotation.bind(py).clone()),
        None => annotation.bind(py).clone(),
    };
    registry.field(&annotation, "validation", Some(title))
}

fn with_schema_extra(py: Python<'_>, mut schema: JsonValue, extra: &Bound<'_, PyAny>) -> JsonValue {
    if let Ok(extra) = extra.cast::<PyDict>() {
        if let (Some(object), Some(extra)) = (
            schema.as_object_mut(),
            py_any_to_json(py, extra).as_object(),
        ) {
            for (key, value) in extra {
                object.insert(key.clone(), value.clone());
            }
        }
        return schema;
    }
    let target = json_to_py_object(py, &schema);
    if extra.call1((target.bind(py),)).is_err() {
        return schema;
    }
    py_any_to_json(py, target.bind(py))
}

fn annotated_discriminator<'py>(
    py: Python<'py>,
    param: &ParsedParameter,
) -> Option<Bound<'py, PyAny>> {
    let annotation = marker_attr(py, param, intern!(py, "annotation"))?;
    let metadata = py
        .import(intern!(py, "typing"))
        .ok()?
        .call_method1(intern!(py, "get_args"), (annotation,))
        .ok()?;
    metadata
        .try_iter()
        .ok()?
        .skip(1)
        .flatten()
        .find_map(|item| {
            item.getattr(intern!(py, "discriminator"))
                .ok()
                .filter(|value| !value.is_none())
        })
}

fn marker_attr<'py>(
    py: Python<'py>,
    param: &ParsedParameter,
    name: &Bound<'py, PyString>,
) -> Option<Bound<'py, PyAny>> {
    param
        .param_object
        .as_ref()?
        .bind(py)
        .getattr(name)
        .ok()
        .filter(|value| !value.is_none())
}

fn discriminated<'py>(
    py: Python<'py>,
    annotation: &Bound<'py, PyAny>,
    discriminator: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let pydantic = py.import(intern!(py, "pydantic"))?;
    let kwargs = PyDict::new(py);
    kwargs.set_item(intern!(py, "discriminator"), discriminator)?;
    let field = pydantic
        .getattr(intern!(py, "Field"))?
        .call((), Some(&kwargs))?;
    py.import(intern!(py, "typing"))?
        .getattr(intern!(py, "Annotated"))?
        .get_item((annotation, field))
}

fn build_request_body(
    py: Python<'_>,
    route: &RouteEntry,
    body_params: &[&ParsedParameter],
    unique_id: &str,
    schemas: &mut HashMap<String, JsonValue>,
    registry: &mut SchemaRegistry,
) -> Option<RequestBody> {
    if body_params.is_empty() || matches!(route.method, HttpMethod::OPTIONS | HttpMethod::TRACE) {
        return None;
    }
    let is_file = |param: &&ParsedParameter| {
        param.is_file || marker_class(py, param).as_deref() == Some("File")
    };
    let is_form = |param: &&ParsedParameter| {
        is_file(param) || marker_class(py, param).as_deref() == Some("Form")
    };
    let has_file = body_params.iter().any(is_file);
    let has_form = body_params.iter().any(is_form);
    let default_media_type = if has_file {
        "multipart/form-data"
    } else if has_form {
        "application/x-www-form-urlencoded"
    } else {
        "application/json"
    };
    let first = body_params[0];
    let single_model_form = body_params.len() == 1 && has_form && first.is_pydantic_model;
    let embed = body_params.len() > 1 || first.embed || (has_form && !single_model_form);

    let (schema, required, media_type) = if embed {
        let mut properties = Object::new();
        let mut required_fields = Vec::new();
        for param in body_params {
            properties.insert(
                param.external_name.clone(),
                body_field_schema(py, param, registry),
            );
            if param.required || !param.has_default {
                required_fields.push(json!(param.external_name.as_str()));
            }
        }
        let wrapper_name = format!("Body_{unique_id}");
        let mut wrapper = json!({
            "properties": properties,
            "type": "object",
            "title": wrapper_name.as_str(),
        });
        if !required_fields.is_empty()
            && let Some(object) = wrapper.as_object_mut()
        {
            object.insert(
                "required".to_string(),
                JsonValue::Array(Box::new(required_fields.clone())),
            );
        }
        schemas.insert(wrapper_name.clone(), wrapper);
        (
            json!({ "$ref": format!("#/components/schemas/{wrapper_name}") }),
            !required_fields.is_empty(),
            default_media_type.to_string(),
        )
    } else {
        let media_type = first
            .param_object
            .as_ref()
            .and_then(|marker| marker.bind(py).getattr(intern!(py, "media_type")).ok())
            .and_then(|value| value.extract::<String>().ok())
            .filter(|_| !has_form)
            .unwrap_or_else(|| default_media_type.to_string());
        (
            body_field_schema(py, first, registry),
            first.required || !first.has_default,
            media_type,
        )
    };

    let examples = (!embed)
        .then(|| marker_attr(py, first, intern!(py, "openapi_examples")))
        .flatten()
        .map(|examples| py_any_to_json(py, &examples));
    let mut content = HashMap::new();
    content.insert(media_type, MediaType { schema, examples });
    Some(RequestBody { content, required })
}

pub fn build_paths_from_routes(
    py: Python<'_>,
    collected: &[RouteEntry],
    schemas: &mut HashMap<String, JsonValue>,
    app_responses: Option<&Py<PyAny>>,
    separate_input_output_schemas: bool,
    generate_unique_id_function: Option<&Py<PyAny>>,
    security_schemes: &mut HashMap<String, JsonValue>,
) -> HashMap<String, PathItem> {
    let mut paths: HashMap<String, PathItem> = HashMap::new();
    let mut registry = SchemaRegistry::default();

    for route in collected {
        let overridden;
        let route = match route.route_object.lock().as_ref() {
            Some(object) => {
                let mut entry = route.clone();
                apply_route_overrides(object.bind(py), &mut entry);
                overridden = entry;
                &overridden
            }
            None => route,
        };
        if !route.include_in_schema {
            continue;
        }

        let path = strip_path_convertors(&route.path);

        let handler = route.handler.clone();
        let tags = &route.tags;

        let description = endpoint_description(py, None, handler.execution.func.bind(py));

        let summary = route
            .summary
            .clone()
            .or_else(|| Some(schema_title(&route.name)));
        let unique_id_function = route
            .generate_unique_id_function
            .as_ref()
            .or(generate_unique_id_function);
        let operation_id = route.operation_id.clone().unwrap_or_else(|| {
            unique_id_function
                .and_then(|function| {
                    let route_obj = route_object(py, route, &handler)?;
                    function
                        .bind(py)
                        .call1((route_obj,))
                        .ok()?
                        .extract::<String>()
                        .ok()
                })
                .unwrap_or_else(|| {
                    native_operation_id(
                        &route.name,
                        &strip_path_convertors(&route.path),
                        route.method.as_ref(),
                    )
                })
        });

        let mut operation = Operation {
            summary,
            description: route
                .description
                .clone()
                .filter(|text| !text.is_empty())
                .or(description),
            operation_id: Some(operation_id.clone()),
            tags: (!tags.is_empty()).then(|| tags.clone()),
            deprecated: route.deprecated,
            parameters: None,
            request_body: None,
            responses: HashMap::new(),
            callbacks: None,
            security: None,
        };

        if !route.security.is_empty() {
            let mut requirements: Vec<JsonValue> = Vec::new();
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
                let mut requirement = Object::new();
                requirement.insert(compiled.name.clone(), json!(scope_list));
                let requirement = JsonValue::Object(Box::new(requirement));
                if !requirements.contains(&requirement) {
                    requirements.push(requirement);
                }
            }
            operation.security = Some(requirements);
        }

        let mut seen_params = std::collections::HashSet::new();
        let mut all_params: Vec<&ParsedParameter> = handler
            .payload
            .parsed_params
            .iter()
            .chain(
                handler
                    .payload
                    .dependencies
                    .iter()
                    .flat_map(|node| node.injection_plan.iter())
                    .filter_map(|(_, injection)| match injection {
                        InjectionType::Parameter(param) => Some(param.as_ref()),
                        _ => None,
                    }),
            )
            .filter(|param| seen_params.insert((param.source.clone(), param.external_name.clone())))
            .collect();
        all_params.sort_by_key(|param| match param.source {
            ParameterSource::Path => 0,
            ParameterSource::Query => 1,
            ParameterSource::Header => 2,
            ParameterSource::Cookie => 3,
            ParameterSource::Body
            | ParameterSource::BackgroundTasks
            | ParameterSource::SecurityScopes => 4,
        });
        let parameters: Vec<Parameter> = all_params
            .iter()
            .filter_map(|param| {
                let location = match param.source {
                    ParameterSource::Path => "path",
                    ParameterSource::Query => "query",
                    ParameterSource::Header => "header",
                    ParameterSource::Cookie => "cookie",
                    ParameterSource::Body
                    | ParameterSource::BackgroundTasks
                    | ParameterSource::SecurityScopes => return None,
                };

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

                if let Some(examples) = marker
                    .and_then(|marker| marker.getattr(intern!(py, "examples")).ok())
                    .filter(|value| !value.is_none())
                    && let Some(obj) = schema.as_object_mut()
                {
                    obj.insert("examples".to_string(), py_any_to_json(py, &examples));
                }

                let is_ref = schema
                    .as_object()
                    .is_some_and(|obj| obj.contains_key("$ref"));
                if !is_ref && let Some(obj) = schema.as_object_mut() {
                    if let Some(title) = marker
                        .and_then(|marker| marker.getattr(intern!(py, "title")).ok())
                        .and_then(|value| value.extract::<String>().ok())
                    {
                        obj.insert("title".to_string(), json!(title));
                    } else if !obj.contains_key("title") {
                        obj.insert(
                            "title".to_string(),
                            json!(schema_title(&param.external_name)),
                        );
                    }
                    if param.has_default
                        && let Some(default) = &param.default_value
                        && !default.bind(py).is_none()
                    {
                        let default = py
                            .import(intern!(py, "pydantic_core"))
                            .and_then(|core| {
                                core.call_method1(intern!(py, "to_jsonable_python"), (default,))
                            })
                            .unwrap_or_else(|_| default.bind(py).clone());
                        obj.insert("default".to_string(), py_any_to_json(py, &default));
                    }
                }

                let mut schema =
                    apply_parameter_constraints(schema, &param.constraints, param.is_list);

                let deprecated = marker
                    .and_then(|marker| marker.getattr(intern!(py, "deprecated")).ok())
                    .filter(|value| !value.is_none())
                    .and_then(|value| value.is_truthy().ok())
                    .filter(|truthy| *truthy)
                    .map(|_| true);
                if !is_ref && let Some(obj) = schema.as_object_mut() {
                    if let Some(description) = &param.description {
                        obj.insert("description".to_string(), json!(description));
                    }
                    if deprecated.is_some() {
                        obj.insert("deprecated".to_string(), json!(true));
                    }
                }
                if !is_ref
                    && let Some(extra) = marker
                        .and_then(|marker| marker.getattr(intern!(py, "json_schema_extra")).ok())
                        .filter(|extra| !extra.is_none())
                {
                    schema = with_schema_extra(py, schema, &extra);
                }
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

        let body_params: Vec<&ParsedParameter> = handler
            .payload
            .dependencies
            .iter()
            .flat_map(|node| node.injection_plan.iter())
            .filter_map(|(_, injection)| match injection {
                InjectionType::Parameter(param) => Some(param.as_ref()),
                _ => None,
            })
            .chain(handler.payload.parsed_params.iter())
            .filter(|param| param.source == ParameterSource::Body)
            .collect();
        operation.request_body = build_request_body(
            py,
            route,
            &body_params,
            &operation_id,
            schemas,
            &mut registry,
        );
        let response_desc = route
            .response_description
            .clone()
            .unwrap_or_else(|| "Successful Response".to_string());

        let response_schema = handler
            .response
            .response_model
            .as_ref()
            .filter(|model| !model.bind(py).is_none())
            .map_or_else(
                || json!({}),
                |model| {
                    registry.field(
                        model.bind(py),
                        "serialization",
                        Some(schema_title(&format!("Response_{operation_id}"))),
                    )
                },
            );

        let success_status = handler
            .response
            .default_status
            .map_or(200, |status| status.as_u16());
        let body_allowed = fastrapi_http::status::body_allowed(success_status);
        let media_type = match handler.response.response_type {
            ResponseType::Html => Some("text/html"),
            ResponseType::PlainText => Some("text/plain"),
            ResponseType::Redirect | ResponseType::File => None,
            ResponseType::Json | ResponseType::Auto => Some("application/json"),
        };
        let response_schema = if media_type == Some("application/json") {
            response_schema
        } else {
            json!({ "type": "string" })
        };
        operation.responses.insert(
            success_status.to_string(),
            Response {
                description: response_desc,
                content: media_type.filter(|_| body_allowed).map(|media_type| {
                    let mut content = HashMap::new();
                    content.insert(
                        media_type.to_string(),
                        MediaType {
                            examples: None,
                            schema: response_schema,
                        },
                    );
                    content
                }),
            },
        );

        if !handler.validation.param_validators.is_empty() || !all_params.is_empty() {
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
                                examples: None,
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
            head: None,
            options: None,
            trace: None,
            parameters: None,
        });

        let mut operation_val = to_owned_value(&operation).unwrap_or_else(|_| json!({}));

        if let Some(extra) = &route.openapi_extra {
            deep_merge_json(&mut operation_val, extra.clone());
        }

        let combined_responses = match (app_responses, route.responses.as_ref()) {
            (None, None) => None,
            (Some(only), None) | (None, Some(only)) => Some(only.bind(py).clone()),
            (Some(app_level), Some(route_level)) => {
                let merged = PyDict::new(py);
                if let Ok(app_level) = app_level.bind(py).cast::<PyDict>() {
                    _ = merged.update(app_level.as_mapping());
                }
                if let Ok(route_level) = route_level.bind(py).cast::<PyDict>() {
                    _ = merged.update(route_level.as_mapping());
                }
                Some(merged.into_any())
            }
        };
        if let Some(responses) = combined_responses
            && let Some(processed) = additional_responses(
                py,
                &responses,
                &operation.responses,
                &operation_id,
                &mut registry,
            )
            && let Some(op_obj) = operation_val.as_object_mut()
            && let Some(op_resp) = op_obj.get_mut("responses")
        {
            deep_merge_json(op_resp, processed);
        }

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
            HttpMethod::HEAD => path_item.head = Some(operation_val),
            HttpMethod::OPTIONS => path_item.options = Some(operation_val),
            HttpMethod::TRACE => path_item.trace = Some(operation_val),
        }
    }
    let resolved = registry.resolve(py, separate_input_output_schemas, schemas);
    for path_item in paths.values_mut() {
        for operation in [
            &mut path_item.get,
            &mut path_item.post,
            &mut path_item.put,
            &mut path_item.delete,
            &mut path_item.patch,
            &mut path_item.head,
            &mut path_item.options,
            &mut path_item.trace,
        ]
        .into_iter()
        .flatten()
        {
            substitute(operation, &resolved);
        }
    }
    for schema in schemas.values_mut() {
        substitute(schema, &resolved);
    }
    paths
}

#[must_use]
pub fn endpoint_description(
    py: Python<'_>,
    explicit: Option<&str>,
    endpoint: &Bound<'_, PyAny>,
) -> Option<String> {
    let cleaned;
    let text = if let Some(text) = explicit.filter(|text| !text.is_empty()) {
        text
    } else {
        let doc = endpoint
            .getattr("__doc__")
            .ok()
            .filter(|doc| !doc.is_none())?;
        cleaned = py
            .import("inspect")
            .and_then(|inspect| inspect.call_method1("cleandoc", (doc,)))
            .and_then(|text| text.extract::<String>())
            .ok()?;
        &cleaned
    };
    let text = text.split('\x0c').next().unwrap_or_default().trim();
    (!text.is_empty()).then(|| text.to_owned())
}

pub fn parse_callbacks_to_json(
    py: Python<'_>,
    callbacks_bound: &Bound<'_, PyAny>,
    collect: RouteCollector,
) -> Option<JsonValue> {
    let mut callbacks_map = Object::new();
    let mut dummy_schemas = HashMap::new();
    let mut dummy_security = HashMap::new();

    if let Ok(dict) = callbacks_bound.cast::<PyDict>() {
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
    } else if let Ok(list) = callbacks_bound.try_iter() {
        for item in list.flatten() {
            let Some(collected) = collect(py, &item) else {
                continue;
            };
            let is_router = item.hasattr("include_router").unwrap_or(false);
            for entry in collected {
                let paths = build_paths_from_routes(
                    py,
                    std::slice::from_ref(&entry),
                    &mut dummy_schemas,
                    None,
                    false,
                    None,
                    &mut dummy_security,
                );
                for (path, path_item) in paths {
                    let value = to_owned_value(&path_item).unwrap_or_else(|_| json!({}));
                    if is_router {
                        callbacks_map.insert(path, value);
                    } else if let Some(named) = callbacks_map
                        .get_mut(entry.name.as_str())
                        .and_then(JsonValue::as_object_mut)
                    {
                        named.insert(path, value);
                    } else {
                        let mut named = Object::new();
                        named.insert(path, value);
                        callbacks_map
                            .insert(entry.name.clone(), JsonValue::Object(Box::new(named)));
                    }
                }
            }
        }
    }

    if callbacks_map.is_empty() {
        None
    } else {
        Some(JsonValue::Object(Box::new(callbacks_map)))
    }
}
