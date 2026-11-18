//! Builds the OpenAPI paths object from the routes.
//! It makes the operations, parameters, request bodies, responses, callbacks, and security schemes for each route.

use super::document::{
    MediaType, Operation, Parameter, PathItem, RequestBody, Response, deep_merge_json,
};
use super::registry::{SchemaRegistry, substitute};
use super::responses::ResponseClass;
use super::responses::success_content;
use super::responses::{VALIDATION_ERROR_SCHEMAS, add_validation_error};
use super::schema::{native_operation_id, schema_title};
use super::spec::RouteCollector;
use fastrapi_core::method::HttpMethod;
use fastrapi_core::params::ParameterSource;
use fastrapi_core::path::strip_path_convertors;
use fastrapi_core::security::SecurityKind;
use fastrapi_pyutil::imports::TYPING;
use fastrapi_pyutil::json::py_any_to_json;
use fastrapi_routing::annotations::strip_optional;
use fastrapi_routing::dependencies::InjectionType;
use fastrapi_routing::route::{
    ModelPresence, ParamModel, ParsedParameter, RouteEntry, RouteHandler,
};
use fastrapi_routing::security::{OAuth2PasswordRequestForm, OAuth2PasswordRequestFormStrict};
use indexmap::IndexMap;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};
use simd_json::json;
use simd_json::owned::{Object, Value as JsonValue};
use simd_json::prelude::{ValueAsMutObject, ValueAsScalar};
use simd_json::serde::to_owned_value;
use std::collections::HashSet;

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
        SecurityKind::Model { scheme } => scheme.clone(),
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
    existing: &IndexMap<String, Response>,
    media_type: &str,
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
                    .entry(media_type.to_string())
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

fn model_parameters(
    py: Python<'_>,
    param: &ParsedParameter,
    location: &str,
    registry: &mut SchemaRegistry,
) -> Vec<Parameter> {
    let Some(model) = &param.model else {
        return Vec::new();
    };
    let optional = model.presence.is_some() && !param.required;
    let Ok(fields) = model
        .annotation
        .bind(py)
        .getattr(intern!(py, "model_fields"))
    else {
        return Vec::new();
    };
    let Ok(fields) = fields.cast_into::<PyDict>() else {
        return Vec::new();
    };
    fields
        .values()
        .iter()
        .zip(model.keys.iter())
        .filter_map(|(info, key)| {
            let text = |name| {
                info.getattr(name)
                    .ok()
                    .and_then(|value| value.extract::<Option<String>>().ok())
                    .flatten()
            };
            let alias = key.alias.bind(py).to_str().ok()?;
            let title = text(intern!(py, "title")).unwrap_or_else(|| schema_title(alias));
            let annotation = annotated_field(&info)?;
            let required = info
                .call_method0(intern!(py, "is_required"))
                .and_then(|value| value.extract::<bool>())
                .ok()?;
            let deprecated = info
                .getattr(intern!(py, "deprecated"))
                .ok()
                .and_then(|value| value.is_truthy().ok())
                .filter(|truthy| *truthy);
            Some(Parameter {
                name: key.lookup.clone(),
                location: location.to_string(),
                required: Some(required && !optional),
                schema: Some(registry.field(&annotation, "validation", Some(title))),
                description: text(intern!(py, "description")),
                deprecated,
                example: None,
                examples: None,
            })
        })
        .collect()
}

/// `Annotated[field_info.annotation, field_info]`, the type FastAPI's `ModelField` builds its
/// schema from, so pydantic documents formats, constraints, defaults and extras.
fn annotated_field<'py>(field_info: &Bound<'py, PyAny>) -> Option<Bound<'py, PyAny>> {
    let py = field_info.py();
    TYPING
        .get(py)
        .ok()?
        .getattr(intern!(py, "Annotated"))
        .ok()?
        .get_item((
            field_info.getattr(intern!(py, "annotation")).ok()?,
            field_info,
        ))
        .ok()
}

/// The `example` and `examples` of a parameter or body: like FastAPI, `openapi_examples` wins.
fn field_examples(
    py: Python<'_>,
    param: &ParsedParameter,
) -> (Option<JsonValue>, Option<JsonValue>) {
    let json = |value: Bound<'_, PyAny>| py_any_to_json(py, &value);
    match marker_attr(py, param, intern!(py, "openapi_examples"))
        .filter(|examples| examples.is_truthy().unwrap_or(false))
    {
        Some(examples) => (None, Some(json(examples))),
        None => (
            marker_attr(py, param, intern!(py, "example")).map(json),
            None,
        ),
    }
}

fn marker_media_type(py: Python<'_>, param: &ParsedParameter) -> Option<String> {
    marker_attr(py, param, intern!(py, "media_type"))?
        .extract()
        .ok()
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
    field_schema(py, param, registry, None)
}

fn embedded_field_schema(
    py: Python<'_>,
    param: &ParsedParameter,
    registry: &mut SchemaRegistry,
) -> JsonValue {
    field_schema(py, param, registry, jsonable_default(py, param))
}

fn field_schema(
    py: Python<'_>,
    param: &ParsedParameter,
    registry: &mut SchemaRegistry,
    default: Option<JsonValue>,
) -> JsonValue {
    let title = schema_title(&param.external_name);
    if param.is_file || marker_class(py, param).as_deref() == Some("File") {
        let binary = json!({ "type": "string", "contentMediaType": "application/octet-stream" });
        let mut schema = if param.is_list {
            json!({ "type": "array", "items": binary })
        } else {
            binary
        };
        if param
            .annotation
            .as_ref()
            .is_some_and(|annotation| strip_optional(py, annotation.bind(py)).is_some())
        {
            schema = json!({ "anyOf": [schema, { "type": "null" }] });
        }
        if let Some(object) = schema.as_object_mut() {
            object.insert("title".to_string(), json!(title));
        }
        return schema;
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
    registry.field_with_default(&annotation, "validation", Some(title), default)
}

fn jsonable_default(py: Python<'_>, param: &ParsedParameter) -> Option<JsonValue> {
    let default = param
        .default_value
        .as_ref()
        .filter(|_| param.has_default)?
        .bind(py);
    if default.is_none() {
        return None;
    }
    let default = py
        .import(intern!(py, "pydantic_core"))
        .and_then(|core| core.call_method1(intern!(py, "to_jsonable_python"), (default,)))
        .unwrap_or_else(|_| default.clone());
    Some(py_any_to_json(py, &default))
}

const fn is_required(param: &ParsedParameter) -> bool {
    param.required || !param.has_default
}

fn form_model(param: &ParsedParameter) -> Option<&ParamModel> {
    param
        .model
        .as_deref()
        .filter(|_| param.is_form && !param.embed)
}

fn form_body_schema(
    py: Python<'_>,
    params: &[&ParsedParameter],
    title: &str,
    registry: &mut SchemaRegistry,
) -> JsonValue {
    let mut properties = Object::new();
    let mut required: Vec<JsonValue> = Vec::new();
    let mut conditions = Vec::new();
    for param in params {
        if let Some(model) = form_model(param) {
            push_form_model_conditions(py, param, model, registry, &mut conditions);
            continue;
        }
        let alias = param.external_name.as_str();
        let schema = embedded_field_schema(py, param, registry);
        if properties.contains_key(alias) {
            conditions.push(json!({ "properties": { alias: schema } }));
        } else {
            properties.insert(alias.to_owned(), schema);
        }
        if is_required(param) && !required.iter().any(|name| name.as_str() == Some(alias)) {
            required.push(json!(alias));
        }
    }
    let mut schema = Object::new();
    schema.insert("type".to_owned(), json!("object"));
    schema.insert("title".to_owned(), json!(title));
    if !properties.is_empty() {
        schema.insert("properties".to_owned(), JsonValue::from(properties));
    }
    if !required.is_empty() {
        schema.insert("required".to_owned(), JsonValue::Array(Box::new(required)));
    }
    schema.insert("allOf".to_owned(), JsonValue::Array(Box::new(conditions)));
    JsonValue::from(schema)
}

fn push_form_model_conditions(
    py: Python<'_>,
    param: &ParsedParameter,
    model: &ParamModel,
    registry: &mut SchemaRegistry,
    conditions: &mut Vec<JsonValue>,
) {
    let title = marker_attr(py, param, intern!(py, "title"))
        .and_then(|title| title.extract::<String>().ok())
        .unwrap_or_else(|| schema_title(&param.name));
    let annotation = model.annotation.bind(py);
    let aliases = model.schema_aliases.as_ref();
    let Some(presence) = model.presence else {
        conditions.push(registry.form_model(annotation, Some(title), aliases));
        return;
    };
    let schema = if model.union {
        registry.field(annotation, "validation", Some(title))
    } else {
        json!({ "anyOf": [registry.form_model(annotation, None, aliases)], "title": title })
    };
    let absent = absent_form_model(model, presence);
    if is_required(param) {
        conditions.push(json!({ "not": absent }));
        conditions.push(schema);
    } else {
        conditions.push(json!({ "anyOf": [absent, schema] }));
    }
}

fn absent_form_model(model: &ParamModel, presence: ModelPresence) -> JsonValue {
    if presence == ModelPresence::AnyKey {
        return json!({ "maxProperties": 0 });
    }
    let mut names: Vec<&str> = model.keys.iter().map(|key| key.lookup.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    if names.is_empty() {
        return json!({});
    }
    let required: Vec<JsonValue> = names
        .into_iter()
        .map(|name| json!({ "required": [name] }))
        .collect();
    json!({ "not": { "anyOf": required } })
}

fn embedded_body_schema(
    py: Python<'_>,
    params: &[&ParsedParameter],
    title: &str,
    registry: &mut SchemaRegistry,
) -> JsonValue {
    let mut properties = Object::new();
    let mut required = Vec::new();
    for param in params {
        properties.insert(
            param.external_name.clone(),
            embedded_field_schema(py, param, registry),
        );
        if is_required(param) {
            required.push(json!(param.external_name.as_str()));
        }
    }
    let mut schema = json!({ "properties": properties, "type": "object", "title": title });
    if !required.is_empty()
        && let Some(object) = schema.as_object_mut()
    {
        object.insert("required".to_string(), JsonValue::Array(Box::new(required)));
    }
    schema
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
    schemas: &mut IndexMap<String, JsonValue>,
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
    let single_model_form = body_params.len() == 1
        && has_form
        && (first.is_pydantic_model || form_model(first).is_some());
    let embed = body_params.len() > 1 || first.embed || (has_form && !single_model_form);
    let mut form_models = body_params
        .iter()
        .filter_map(|param| form_model(param))
        .peekable();
    let composed = form_models.peek().is_some()
        && (embed || form_models.any(|model| model.presence.is_some()));

    let (schema, required, media_type) = if embed || composed {
        let wrapper_name = format!("Body_{unique_id}");
        let wrapper = if composed {
            form_body_schema(py, body_params, &wrapper_name, registry)
        } else {
            embedded_body_schema(py, body_params, &wrapper_name, registry)
        };
        schemas.insert(wrapper_name.clone(), wrapper);
        let media_types: Vec<String> = body_params
            .iter()
            .filter(|param| !has_form || is_form(param))
            .filter_map(|param| marker_media_type(py, param))
            .collect();
        let media_type = if has_file
            || (has_form
                && media_types
                    .iter()
                    .any(|media| media == "multipart/form-data"))
        {
            "multipart/form-data".to_string()
        } else {
            match media_types.split_first() {
                Some((media, rest)) if rest.iter().all(|other| other == media) => media.clone(),
                _ => default_media_type.to_string(),
            }
        };
        (
            json!({ "$ref": format!("#/components/schemas/{wrapper_name}") }),
            body_params.iter().any(|param| is_required(param)),
            media_type,
        )
    } else {
        let media_type =
            marker_media_type(py, first).unwrap_or_else(|| default_media_type.to_string());
        (
            body_field_schema(py, first, registry),
            first.required || !first.has_default,
            media_type,
        )
    };

    let (example, examples) = if embed {
        (None, None)
    } else {
        field_examples(py, first)
    };
    let mut content = IndexMap::new();
    content.insert(
        media_type,
        MediaType {
            schema: Some(schema),
            example,
            examples,
            ..MediaType::default()
        },
    );
    Some(RequestBody { content, required })
}

/// The form body of the OAuth2 password form classes. Their `__new__` is Rust, so
/// no parameters are parsed from them, and FastAPI's fields are written out here.
fn password_form_body(
    operation_id: &str,
    strict: bool,
    schemas: &mut IndexMap<String, JsonValue>,
) -> RequestBody {
    let name = format!("Body_{operation_id}");
    let nullable = |title: &str| json!({ "anyOf": [{ "type": "string" }, { "type": "null" }], "title": title });
    let (mut grant_type, mut password, mut client_secret, mut required) = (
        json!({ "pattern": "^password$", "title": "Grant Type", "type": "string" }),
        json!({ "title": "Password", "type": "string" }),
        nullable("Client Secret"),
        vec![json!("grant_type"), json!("username"), json!("password")],
    );
    if !strict {
        grant_type = json!({
            "anyOf": [{ "pattern": "^password$", "type": "string" }, { "type": "null" }],
            "title": "Grant Type",
        });
        for schema in [&mut password, &mut client_secret] {
            if let Some(object) = schema.as_object_mut() {
                object.insert("format".to_owned(), json!("password"));
            }
        }
        required.remove(0);
    }
    let schema = json!({
        "properties": {
            "grant_type": grant_type,
            "username": { "title": "Username", "type": "string" },
            "password": password,
            "scope": { "default": "", "title": "Scope", "type": "string" },
            "client_id": nullable("Client Id"),
            "client_secret": client_secret,
        },
        "required": required,
        "title": name,
        "type": "object",
    });
    schemas.insert(name.clone(), schema);
    let media = MediaType {
        schema: Some(json!({ "$ref": format!("#/components/schemas/{name}") })),
        ..MediaType::default()
    };
    RequestBody {
        content: IndexMap::from([("application/x-www-form-urlencoded".to_owned(), media)]),
        required: true,
    }
}

/// The operation ids of a document. Like FastAPI's `operation_ids`, a repeated id is reported
/// with a warning that names the endpoint.
#[derive(Default)]
pub struct OperationIds {
    seen: HashSet<String>,
    duplicates: Vec<(String, Py<PyAny>)>,
}

impl OperationIds {
    fn record(&mut self, py: Python<'_>, operation_id: &str, endpoint: &Py<PyAny>) {
        if !self.seen.insert(operation_id.to_owned()) {
            self.duplicates
                .push((operation_id.to_owned(), endpoint.clone_ref(py)));
        }
    }

    /// Emits the `UserWarning` FastAPI gives for each repeated operation id.
    pub fn warn(&self, py: Python<'_>) -> PyResult<()> {
        for (operation_id, endpoint) in &self.duplicates {
            let endpoint = endpoint.bind(py);
            let name = endpoint
                .getattr(intern!(py, "__name__"))
                .map_or_else(|_| "<unnamed_endpoint>".to_owned(), |name| name.to_string());
            let mut message = format!("Duplicate Operation ID {operation_id} for function {name}");
            if let Ok(globals) = endpoint.getattr(intern!(py, "__globals__"))
                && let Ok(file) = globals.call_method1(intern!(py, "get"), ("__file__",))
                && file.is_truthy()?
            {
                message = format!("{message} at {file}");
            }
            PyErr::warn(
                py,
                &py.get_type::<pyo3::exceptions::PyUserWarning>(),
                &std::ffi::CString::new(message)?,
                1,
            )?;
        }
        Ok(())
    }
}

/// The application settings that the operations of one document share.
#[derive(Clone, Copy)]
pub struct PathSettings<'a> {
    pub app_responses: Option<&'a Py<PyAny>>,
    pub separate_input_output_schemas: bool,
    pub generate_unique_id_function: Option<&'a Py<PyAny>>,
    pub collect: RouteCollector,
}

pub fn build_paths_from_routes(
    py: Python<'_>,
    collected: &[RouteEntry],
    schemas: &mut IndexMap<String, JsonValue>,
    security_schemes: &mut IndexMap<String, JsonValue>,
    operation_ids: &mut OperationIds,
    settings: PathSettings<'_>,
) -> IndexMap<String, PathItem> {
    let mut paths: IndexMap<String, PathItem> = IndexMap::new();
    let mut registry = SchemaRegistry::default();

    for route in collected {
        let overridden;
        // The object is cloned out first: reading its attributes calls Python, which must not run under the lock.
        let object = route
            .route_object
            .lock()
            .as_ref()
            .map(|object| object.clone_ref(py));
        let route = match object {
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
            .filter(|summary| !summary.is_empty())
            .or_else(|| Some(schema_title(&route.name)));
        let unique_id_function = route
            .generate_unique_id_function
            .as_ref()
            .or(settings.generate_unique_id_function);
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

        operation_ids.record(py, &operation_id, &handler.execution.func);

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
            responses: IndexMap::new(),
            callbacks: None,
            security: None,
        };

        if !route.security.is_empty() {
            // Like FastAPI, the scopes of one scheme merge into a single requirement, and the
            // last definition of a scheme name wins.
            let mut requirements: IndexMap<&str, Vec<&str>> = IndexMap::new();
            for req in &route.security {
                let compiled = req.scheme.as_ref();
                security_schemes.insert(
                    compiled.name.clone(),
                    scheme_to_openapi(
                        &compiled.kind,
                        compiled.description.as_deref(),
                        compiled.scopes.as_ref(),
                    ),
                );
                let scopes = requirements.entry(compiled.name.as_str()).or_default();
                for scope in req.scopes.iter() {
                    if !scopes.contains(&scope.as_str()) {
                        scopes.push(scope);
                    }
                }
            }
            operation.security = Some(
                requirements
                    .into_iter()
                    .map(|(name, scopes)| json!({ name: scopes }))
                    .collect(),
            );
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
        let password_form = handler.payload.dependencies.iter().find_map(|node| {
            let func = node.func.bind(py);
            func.is_instance_of::<OAuth2PasswordRequestForm>()
                .then(|| func.is_instance_of::<OAuth2PasswordRequestFormStrict>())
        });
        // FastAPI documents a 422 response only for routes that read request parameters or a body.
        let validates_request = password_form.is_some()
            || all_params.iter().any(|param| {
                !matches!(
                    param.source,
                    ParameterSource::BackgroundTasks | ParameterSource::SecurityScopes
                )
            });
        let parameters: Vec<Parameter> = all_params
            .iter()
            .flat_map(|param| {
                let location = match param.source {
                    ParameterSource::Path => "path",
                    ParameterSource::Query => "query",
                    ParameterSource::Header => "header",
                    ParameterSource::Cookie => "cookie",
                    ParameterSource::Body
                    | ParameterSource::BackgroundTasks
                    | ParameterSource::SecurityScopes => return Vec::new(),
                };

                let Some(marker) = param.param_object.as_ref().map(|marker| marker.bind(py)) else {
                    return Vec::new();
                };
                let include_in_schema = marker
                    .getattr(intern!(py, "include_in_schema"))
                    .and_then(|value| value.extract::<bool>())
                    .unwrap_or(true);
                if !include_in_schema {
                    return Vec::new();
                }
                if param.model.is_some() {
                    return model_parameters(py, param, location, &mut registry);
                }

                let title = marker_attr(py, param, intern!(py, "title"))
                    .and_then(|title| title.extract::<String>().ok())
                    .unwrap_or_else(|| schema_title(&param.external_name));
                let schema = match annotated_field(marker) {
                    Some(annotation) => registry.field(&annotation, "validation", Some(title)),
                    None => json!({ "title": title }),
                };
                let deprecated = marker
                    .getattr(intern!(py, "deprecated"))
                    .and_then(|value| value.is_truthy())
                    .is_ok_and(|truthy| truthy)
                    .then_some(true);
                let (example, examples) = field_examples(py, param);

                vec![Parameter {
                    name: param.external_name.clone(),
                    location: location.to_string(),
                    required: Some(param.required || location == "path"),
                    schema: Some(schema),
                    description: param.description.clone(),
                    deprecated,
                    example,
                    examples,
                }]
            })
            .collect();

        operation.parameters = (!parameters.is_empty()).then_some(parameters);

        // Like FastAPI, the endpoint's own body fields come before those of its dependencies.
        let body_params: Vec<&ParsedParameter> = handler
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
        if let (None, Some(strict)) = (&operation.request_body, password_form) {
            operation.request_body = Some(password_form_body(&operation_id, strict, schemas));
        }
        let response_desc = route
            .response_description
            .clone()
            .unwrap_or_else(|| "Successful Response".to_string());

        let response_class = ResponseClass::of(py, handler.response.response_class.as_ref());
        let success_status = handler
            .response
            .default_status
            .map_or(response_class.status, |status| status.as_u16());
        let body_allowed = handler
            .response
            .default_status
            .is_none_or(|status| fastrapi_http::status::body_allowed(status.as_u16()));
        let content = body_allowed
            .then(|| success_content(py, &handler, &response_class, &operation_id, &mut registry))
            .flatten();
        operation.responses.insert(
            success_status.to_string(),
            Response {
                description: response_desc,
                content,
            },
        );

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

        let combined_responses = match (settings.app_responses, route.responses.as_ref()) {
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
                response_class
                    .media_type
                    .as_deref()
                    .unwrap_or("application/json"),
                &operation_id,
                &mut registry,
            )
            && let Some(op_obj) = operation_val.as_object_mut()
            && let Some(op_resp) = op_obj.get_mut("responses")
        {
            deep_merge_json(op_resp, processed);
        }

        if validates_request
            && let Some(responses) = operation_val
                .as_object_mut()
                .and_then(|operation| operation.get_mut("responses"))
                .and_then(ValueAsMutObject::as_object_mut)
        {
            add_validation_error(responses, schemas);
        }

        let own_callbacks = route.callback_routes.as_ref().and_then(|callbacks| {
            let mut callback_schemas = IndexMap::new();
            let document = callbacks_document(
                py,
                callbacks.bind(py),
                settings.collect,
                &mut callback_schemas,
            );
            // Like FastAPI, a callback's own 422 adds no components.
            for (name, schema) in callback_schemas
                .into_iter()
                .filter(|(name, _)| !VALIDATION_ERROR_SCHEMAS.contains(&name.as_str()))
            {
                schemas.entry(name).or_insert(schema);
            }
            document
        });
        let callbacks = match (route.callbacks.clone(), own_callbacks) {
            (Some(mut inherited), Some(own)) => {
                deep_merge_json(&mut inherited, own);
                Some(inherited)
            }
            (inherited, own) => inherited.or(own),
        };
        if let Some(callbacks) = callbacks
            && let Some(op_obj) = operation_val.as_object_mut()
        {
            op_obj.insert("callbacks".to_string(), callbacks);
        }

        if let Some(extra) = &route.openapi_extra {
            deep_merge_json(&mut operation_val, extra.clone());
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
    let resolved = registry.resolve(py, settings.separate_input_output_schemas, schemas);
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
    callbacks_document(py, callbacks_bound, collect, &mut IndexMap::new())
}

/// The `callbacks` object of an operation. The schemas of the callback models go into `schemas`.
/// Like FastAPI, the security schemes of callbacks are not documented.
fn callbacks_document(
    py: Python<'_>,
    callbacks_bound: &Bound<'_, PyAny>,
    collect: RouteCollector,
    schemas: &mut IndexMap<String, JsonValue>,
) -> Option<JsonValue> {
    let mut callbacks_map = Object::new();
    let mut dummy_security = IndexMap::new();
    let settings = PathSettings {
        app_responses: None,
        separate_input_output_schemas: false,
        generate_unique_id_function: None,
        collect,
    };

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
                            schemas,
                            &mut dummy_security,
                            &mut OperationIds::default(),
                            settings,
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
                    schemas,
                    &mut dummy_security,
                    &mut OperationIds::default(),
                    settings,
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
