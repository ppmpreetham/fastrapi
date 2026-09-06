use crate::{
    FastrAPI,
    decorators::PyAPIRouter,
    ffi::pydantic,
    routing::types::{ParameterConstraints, ParameterSource, RouteEntry},
    types::route::{HttpMethod, RouteHandler},
    utils::{py_any_to_json, py_dict_to_json},
};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString, PyType};
use serde::{Deserialize, Serialize};
use simd_json::json;
use simd_json::owned::Value as JsonValue;
use simd_json::prelude::*;
use std::collections::HashMap;
crate::cached_py_import!(ENUM_MODULE, "enum");
crate::cached_py_import!(TYPING_MODULE, "typing");
crate::cached_py_import!(TYPING_GET_ORIGIN, "typing", "get_origin");
use tracing::debug;

pub fn deep_merge_json(target: &mut JsonValue, source: JsonValue) {
    if source.is_object() && target.is_object() {
        let source_obj = source.as_object().expect("Source must be an object");
        let target_obj = target.as_object_mut().expect("Target must be an object");

        for (k, v) in source_obj.iter() {
            if target_obj.contains_key(k.as_str()) {
                let target_val = target_obj.get_mut(k).expect("Key not found in target");
                deep_merge_json(target_val, v.clone());
            } else {
                target_obj.insert(k.clone(), v.clone());
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

pub fn extract_pydantic_schema(py: Python, model: &Bound<PyAny>, mode: &str) -> Option<JsonValue> {
    // Pydantic v2
    if let Ok(schema_method) = model.getattr("model_json_schema") {
        let kwargs = pyo3::types::PyDict::new(py);
        _ = kwargs.set_item("mode", mode);
        if let Ok(result) = schema_method.call((), Some(&kwargs))
            && let Ok(dict) = result.cast::<PyDict>()
        {
            // fastapi omits `default: null` entries pydantic emits for
            // `Optional[...] = None` fields
            let mut schema = py_dict_to_json(py, dict);
            strip_null_defaults(&mut schema);
            return Some(schema);
        }
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

/// recursively removes `"default": null` entries from a json schema
fn strip_null_defaults(schema: &mut JsonValue) {
    if schema.is_object() {
        let drop_default = schema
            .get("default")
            .map(|value| value.is_null())
            .unwrap_or(false);
        if drop_default && let Some(obj) = schema.as_object_mut() {
            obj.remove(&"default".to_string());
        }
        if let Some(obj) = schema.as_object_mut() {
            for (_, value) in obj.iter_mut() {
                strip_null_defaults(value);
            }
        }
    } else if schema.is_array()
        && let Some(arr) = schema.as_array_mut()
    {
        for value in arr.iter_mut() {
            strip_null_defaults(value);
        }
    }
}

fn get_schema_name(model: &Bound<PyAny>) -> String {
    model
        .getattr("__name__")
        .ok()
        .and_then(|name| name.extract::<String>().ok())
        .unwrap_or_else(|| "UnknownSchema".to_string())
}

/// `typing.get_origin(x) is typing.Literal`
fn is_literal(py: Python<'_>, type_hint: &Bound<PyAny>) -> bool {
    TYPING_GET_ORIGIN
        .get(py)
        .ok()
        .and_then(|get_origin| get_origin.call1((type_hint,)).ok())
        .and_then(|origin| {
            TYPING_MODULE
                .get(py)
                .ok()?
                .getattr(intern!(py, "Literal"))
                .ok()
                .map(|literal| origin.is(&literal))
        })
        .unwrap_or(false)
}

/// is the hint an `enum.Enum` subclass?
fn is_enum_class(py: Python<'_>, type_hint: &Bound<PyAny>) -> bool {
    type_hint
        .cast::<PyType>()
        .ok()
        .and_then(|ty| {
            ENUM_MODULE
                .get(py)
                .ok()?
                .getattr(intern!(py, "Enum"))
                .ok()
                .and_then(|base| ty.is_subclass(base.as_any()).ok())
        })
        .unwrap_or(false)
}

/// pydantic-style field title: `item_id` → "Item Id", `user-agent` → "User-Agent"
fn schema_title(name: &str) -> String {
    let name = name.replace('_', " ");
    let mut out = String::with_capacity(name.len());
    let mut capitalize = true;
    for ch in name.chars() {
        if ch.is_alphabetic() {
            if capitalize {
                out.extend(ch.to_uppercase());
            } else {
                out.extend(ch.to_lowercase());
            }
            capitalize = false;
        } else {
            out.push(ch);
            capitalize = true;
        }
    }
    out
}

/// fastapi's combined-body schema title: the operation id prefixed with `Body_`
fn body_model_name(handler_func: &Bound<'_, pyo3::PyAny>, path: &str, method: &str) -> String {
    let func_name = handler_func
        .getattr("__name__")
        .ok()
        .and_then(|name| name.extract::<String>().ok())
        .unwrap_or_else(|| "unknown".to_string());
    format!("Body_{}", native_operation_id(&func_name, path, method))
}

/// fastapi's default operation id: `{name}{path with \W → _}_{method}`
fn native_operation_id(name: &str, path: &str, method: &str) -> String {
    let joined = format!("{name}{path}");
    let sanitized: String = joined
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{sanitized}_{}", method.to_lowercase())
}

fn python_type_to_openapi_type(
    py: Python<'_>,
    type_hint: &Bound<PyAny>,
    schemas: &mut HashMap<String, JsonValue>,
) -> JsonValue {
    if pydantic::is_pydantic_model(py, type_hint) {
        let schema_name = get_schema_name(type_hint);
        return json!({
            "$ref": format!("#/components/schemas/{schema_name}")
        });
    }

    // `Optional[X]` / `Union[A, B]` → anyOf, the way pydantic renders them
    if let Some(args) = pydantic::union_args(py, type_hint) {
        let has_none = args.iter().any(|arg| pydantic::is_none_type(&arg));
        let mut any_of: Vec<JsonValue> = args
            .iter()
            .filter(|arg| !pydantic::is_none_type(arg))
            .map(|arg| python_type_to_openapi_type(py, &arg, schemas))
            .collect();
        if !any_of.is_empty() {
            if has_none {
                any_of.push(json!({ "type": "null" }));
            }
            return json!({ "anyOf": any_of });
        }
    }

    // `Literal["x", "y"]` → enum schema
    if is_literal(py, type_hint) {
        let values: Vec<JsonValue> = type_hint
            .getattr("__args__")
            .ok()
            .and_then(|args| args.try_iter().ok())
            .map(|iter| {
                iter.filter_map(|a| a.ok())
                    .map(|a| py_any_to_json(py, &a))
                    .collect()
            })
            .unwrap_or_default();
        if !values.is_empty() {
            let all_str = values.iter().all(|v| v.is_str());
            let all_int = values.iter().all(|v| v.is_i64());
            let mut schema = json!({ "enum": values });
            if let Some(obj) = schema.as_object_mut() {
                if all_str {
                    obj.insert("type".to_string(), json!("string"));
                } else if all_int {
                    obj.insert("type".to_string(), json!("integer"));
                }
            }
            return schema;
        }
    }

    // enum classes get a components schema and a `$ref`, like pydantic models
    if is_enum_class(py, type_hint) {
        let schema_name = get_schema_name(type_hint);
        schemas.entry(schema_name.clone()).or_insert_with(|| {
            let values: Vec<JsonValue> = type_hint
                .try_iter()
                .ok()
                .map(|iter| {
                    iter.filter_map(|member| {
                        let member = member.ok()?;
                        let value = member.getattr(intern!(py, "value")).ok()?;
                        Some(py_any_to_json(py, &value))
                    })
                    .collect()
                })
                .unwrap_or_default();
            let all_str = values.iter().all(|v| v.is_str());
            let all_int = values.iter().all(|v| v.is_i64());
            let mut schema = json!({ "enum": values, "title": schema_name });
            if let Some(obj) = schema.as_object_mut() {
                if all_str {
                    obj.insert("type".to_string(), json!("string"));
                } else if all_int {
                    obj.insert("type".to_string(), json!("integer"));
                }
            }
            schema
        });
        return json!({ "$ref": format!("#/components/schemas/{schema_name}") });
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
            "list" | "set" | "tuple" => {
                let items = type_hint
                    .getattr("__args__")
                    .ok()
                    .and_then(|args| args.get_item(0).ok())
                    .map(|elem| python_type_to_openapi_type(py, &elem, schemas))
                    .unwrap_or_else(|| json!({ "type": "string" }));
                return json!({ "type": "array", "items": items });
            }
            "dict" => return json!({ "type": "object" }),
            _ => {}
        }
    }

    if let Ok(type_repr) = type_hint.str()
        && let Ok(type_name) = type_repr.to_str()
    {
        if type_name.contains("List") || type_name.contains("list") {
            let items = type_hint
                .getattr("__args__")
                .ok()
                .and_then(|args| args.get_item(0).ok())
                .map(|elem| python_type_to_openapi_type(py, &elem, schemas))
                .unwrap_or_else(|| json!({ "type": "string" }));
            return json!({ "type": "array", "items": items });
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

/// converts a python list of dicts to a json array, skipping non-dict entries.
fn py_dicts_to_json_vec(py: Python<'_>, value: &Py<PyAny>) -> Option<Vec<JsonValue>> {
    let list = value.extract::<Vec<Py<PyAny>>>(py).ok()?;
    Some(
        list.into_iter()
            .filter_map(|item| {
                item.bind(py)
                    .cast::<PyDict>()
                    .ok()
                    .map(|d| py_dict_to_json(py, d))
            })
            .collect(),
    )
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

    if let Some(servers) = &app.servers {
        spec.servers = py_dicts_to_json_vec(py, servers);
    }

    if app.root_path_in_servers && !app.root_path.is_empty() {
        spec.servers.get_or_insert_with(Vec::new).push(json!({
            "url": app.root_path
        }));
    }

    if let Some(tags) = &app.openapi_tags {
        spec.tags = py_dicts_to_json_vec(py, tags);
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
    let mut security_schemes: HashMap<String, JsonValue> = HashMap::new();

    spec.paths = build_paths_from_routes(
        py,
        &collected,
        &mut schemas,
        app_responses.as_ref(),
        app.separate_input_output_schemas,
        app.generate_unique_id_function.as_ref(),
        &mut security_schemes,
    );

    if let Some(wh) = &app.webhooks
        && let Ok(router) = wh.bind(py).cast::<crate::decorators::PyAPIRouter>()
    {
        let wh_collected = collect_routes(py, &router.borrow());
        let wh_paths = build_paths_from_routes(
            py,
            &wh_collected,
            &mut schemas,
            None,
            app.separate_input_output_schemas,
            app.generate_unique_id_function.as_ref(),
            &mut security_schemes,
        );
        spec.webhooks = Some(wh_paths);
    }

    if let Some(components) = &mut spec.components {
        components.schemas = schemas;
        components.security_schemes = security_schemes;
    }
    debug!("Built OpenAPI spec with {} paths", spec.paths.len());
    simd_json::serde::to_owned_value(&spec).unwrap_or_else(|_| json!({}))
}

/// OpenAPI securitySchemes entry
fn scheme_to_openapi(
    kind: &crate::types::route::SecurityKind,
    description: Option<&str>,
    scopes: Option<&simd_json::OwnedValue>,
) -> JsonValue {
    use crate::types::route::SecurityKind;

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
            .map(|param| param.embed)
            .unwrap_or(false);

        let body_schema = if embedded {
            let param_name = handler
                .payload
                .parsed_params
                .iter()
                .find(|param| param.validator_index == Some(0))
                .map(|param| param.external_name.clone())
                .unwrap_or_else(|| validators[0].name.clone());
            let wrapper_name = body_model_name(
                handler.execution.func.bind(py),
                &strip_converters(&route.path),
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

    let mut properties = simd_json::owned::Object::new();
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
        &strip_converters(&route.path),
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

/// drops the `:convertor` suffix inside `{...}` path templates
fn strip_converters(path: &str) -> String {
    static PATH_PARAM_REGEX: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let regex = PATH_PARAM_REGEX
        .get_or_init(|| regex::Regex::new(r"\{([^}]+)\}").expect("invalid path param regex"));
    regex
        .replace_all(path, |caps: &regex::Captures| {
            format!("{{{}}}", caps[1].split(':').next().unwrap_or(&caps[1]))
        })
        .into_owned()
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
        let path = strip_converters(&route.path);

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
                    &strip_converters(&route.path),
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
            let mut grouped = simd_json::owned::Object::new();
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
            operation.security = Some(vec![
                simd_json::serde::to_owned_value(&grouped).unwrap_or_else(|_| json!({})),
            ]);
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

                let mut schema = param
                    .annotation
                    .as_ref()
                    .map(|annotation| python_type_to_openapi_type(py, annotation.bind(py), schemas))
                    .unwrap_or_else(|| json!({"type": "string"}));

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
                        obj.insert("title".to_string(), json!(schema_title(&param.external_name)));
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
                schemas.insert(schema_name.clone(), schema);
                response_schema =
                    json!({ "$ref": format!("#/components/schemas/{}", schema_name) });
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

        let mut operation_val = simd_json::serde::to_owned_value(&operation).unwrap_or_else(|_| json!({}));

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

pub fn parse_callbacks_to_json(
    py: Python<'_>,
    callbacks_bound: &Bound<'_, PyAny>,
) -> Option<JsonValue> {
    let mut callbacks_map = simd_json::owned::Object::new();
    let mut dummy_schemas = HashMap::new();
    let mut dummy_security = HashMap::new();

    if let Ok(list) = callbacks_bound.try_iter() {
        for item in list.flatten() {
            if let Ok(router_ref) = item.cast::<crate::decorators::PyAPIRouter>() {
                let router = router_ref.borrow();
                let collected = collect_routes(py, &router);
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
                    let value = simd_json::serde::to_owned_value(&path_item).unwrap_or_else(|_| json!({}));
                    callbacks_map.insert(path, value);
                }
            }
        }
    } else if let Ok(dict) = callbacks_bound.cast::<PyDict>() {
        for (k, v) in dict.iter() {
            if let Ok(k_str) = k.extract::<String>()
                && let Ok(list) = v.try_iter()
            {
                let mut inner_map = simd_json::owned::Object::new();
                for item in list.flatten() {
                    if let Ok(router_ref) = item.cast::<crate::decorators::PyAPIRouter>() {
                        let router = router_ref.borrow();
                        let collected = collect_routes(py, &router);
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
                            let value =
                                simd_json::serde::to_owned_value(&path_item).unwrap_or_else(|_| json!({}));
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
        Some(JsonValue::Object(std::boxed::Box::new(callbacks_map)))
    }
}

fn collect_routes(py: Python<'_>, router: &PyAPIRouter) -> Vec<RouteEntry> {
    let flat = router.flatten(py);
    flat.0.clone()
}
