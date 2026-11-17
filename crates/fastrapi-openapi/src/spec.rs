//! Builds the full OpenAPI document of an application.
//! `AppDocument` contains the application settings, and `build_openapi_spec` converts them into a JSON value.

use super::document::OpenApiSpec;
use super::paths::build_paths_from_routes;
use super::paths::{OperationIds, PathSettings};
use super::schema::py_dicts_to_json_vec;
use fastrapi_pyutil::json::{json_to_py_object, py_dict_to_json};
use fastrapi_routing::route::RouteEntry;
use indexmap::IndexMap;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use simd_json::StaticNode;
use simd_json::json;
use simd_json::owned::Value as JsonValue;
use simd_json::prelude::ValueAsArray;
use simd_json::prelude::ValueAsMutObject;
use simd_json::prelude::ValueAsScalar;
use simd_json::prelude::ValueObjectAccess;
use simd_json::serde::to_owned_value;
use tracing::debug;

pub type RouteCollector = fn(Python<'_>, &Bound<'_, PyAny>) -> Option<Vec<RouteEntry>>;

pub struct AppDocument<'a> {
    pub openapi_version: &'a str,
    pub title: &'a str,
    pub version: &'a str,
    pub summary: Option<&'a str>,
    pub description: &'a str,
    pub terms_of_service: Option<&'a str>,
    pub contact: Option<&'a Py<PyAny>>,
    pub license_info: Option<&'a Py<PyAny>>,
    pub servers: Option<&'a Py<PyAny>>,
    pub root_path_in_servers: bool,
    pub root_path: &'a str,
    pub openapi_tags: Option<&'a Py<PyAny>>,
    pub openapi_external_docs: Option<&'a Py<PyAny>>,
    pub responses: Option<&'a Py<PyAny>>,
    pub webhooks: Option<&'a Py<PyAny>>,
    pub generate_unique_id_function: Option<&'a Py<PyAny>>,
    pub separate_input_output_schemas: bool,
    pub routes: &'a [RouteEntry],
}

pub fn build_openapi_spec(
    py: Python<'_>,
    app: &AppDocument<'_>,
    collect: RouteCollector,
) -> PyResult<JsonValue> {
    let mut spec = OpenApiSpec::default();
    app.openapi_version.clone_into(&mut spec.openapi);

    app.title.clone_into(&mut spec.info.title);
    app.version.clone_into(&mut spec.info.version);
    spec.info.summary = app.summary.map(str::to_owned);

    spec.info.description = (!app.description.is_empty()).then(|| app.description.to_owned());
    spec.info.terms_of_service = app.terms_of_service.map(str::to_owned);

    if let Some(contact) = app.contact
        && let Ok(dict) = contact.bind(py).cast::<PyDict>()
    {
        spec.info.contact = Some(py_dict_to_json(py, dict));
    }

    if let Some(license) = app.license_info
        && let Ok(dict) = license.bind(py).cast::<PyDict>()
    {
        spec.info.license = Some(py_dict_to_json(py, dict));
    }

    let mut servers = app
        .servers
        .and_then(|servers| py_dicts_to_json_vec(py, servers))
        .unwrap_or_default();
    if app.root_path_in_servers {
        add_root_path_server(&mut servers, app.root_path);
    }
    spec.servers = (!servers.is_empty()).then_some(servers);

    if let Some(tags) = app.openapi_tags {
        spec.tags = py_dicts_to_json_vec(py, tags);
    }

    if let Some(docs) = app.openapi_external_docs
        && let Ok(dict) = docs.bind(py).cast::<PyDict>()
    {
        spec.external_docs = Some(py_dict_to_json(py, dict));
    }

    let mut schemas: IndexMap<String, JsonValue> = IndexMap::new();
    let mut security_schemes: IndexMap<String, JsonValue> = IndexMap::new();

    let settings = PathSettings {
        app_responses: app.responses,
        separate_input_output_schemas: app.separate_input_output_schemas,
        generate_unique_id_function: app.generate_unique_id_function,
        collect,
    };
    let mut operation_ids = OperationIds::default();
    spec.paths = build_paths_from_routes(
        py,
        app.routes,
        &mut schemas,
        &mut security_schemes,
        &mut operation_ids,
        settings,
    );

    if let Some(webhooks) = app.webhooks
        && let Some(wh_collected) = collect(py, webhooks.bind(py))
    {
        let wh_paths = build_paths_from_routes(
            py,
            &wh_collected,
            &mut schemas,
            &mut security_schemes,
            &mut operation_ids,
            PathSettings {
                app_responses: None,
                ..settings
            },
        );
        spec.webhooks = (!wh_paths.is_empty()).then_some(wh_paths);
    }
    operation_ids.warn(py)?;

    // FastAPI lists component schemas sorted by name.
    schemas.sort_unstable_keys();
    spec.components = (!schemas.is_empty() || !security_schemes.is_empty()).then(|| {
        super::document::Components {
            schemas,
            security_schemes,
        }
    });
    debug!("Built OpenAPI spec with {} paths", spec.paths.len());
    let mut document = to_owned_value(&spec).map_err(|err| {
        PyValueError::new_err(format!("Could not build the OpenAPI document: {err}"))
    })?;
    drop_nulls(&mut document);
    // Like FastAPI, the finished document must satisfy the OpenAPI models.
    py.import("fastrapi.openapi.models")?
        .getattr("OpenAPI")?
        .call_method1("model_validate", (json_to_py_object(py, &document),))?;
    Ok(document)
}

fn drop_nulls(value: &mut JsonValue) {
    match value {
        JsonValue::Object(object) => {
            object.retain(|_, item| !matches!(item, JsonValue::Static(StaticNode::Null)));
            object.values_mut().for_each(drop_nulls);
        }
        JsonValue::Array(items) => items.iter_mut().for_each(drop_nulls),
        _ => {}
    }
}

/// The document served under the ASGI `root_path` of a request, when it differs from `document`.
#[must_use]
pub fn with_root_path_server(document: &JsonValue, root_path: &str) -> Option<JsonValue> {
    let mut servers = document
        .get("servers")
        .and_then(ValueAsArray::as_array)
        .cloned()
        .unwrap_or_default();
    if !add_root_path_server(&mut servers, root_path) {
        return None;
    }
    let mut document = document.clone();
    document
        .as_object_mut()?
        .insert("servers".to_owned(), JsonValue::Array(Box::new(servers)));
    Some(document)
}

/// Lists the root path first in `servers` unless a server already has its URL, like FastAPI's
/// `/openapi.json` route. Tells whether `servers` changed.
fn add_root_path_server(servers: &mut Vec<JsonValue>, root_path: &str) -> bool {
    let root_path = root_path.trim_end_matches('/');
    let listed = servers
        .iter()
        .any(|server| server.get("url").and_then(ValueAsScalar::as_str) == Some(root_path));
    if root_path.is_empty() || listed {
        return false;
    }
    servers.insert(0, json!({ "url": root_path }));
    true
}
