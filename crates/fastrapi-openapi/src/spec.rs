//! Builds the full OpenAPI document of an application.
//! `AppDocument` contains the application settings, and `build_openapi_spec` converts them into a JSON value.

use super::document::OpenApiSpec;
use super::paths::build_paths_from_routes;
use super::schema::py_dicts_to_json_vec;
use fastrapi_pyutil::json::py_dict_to_json;
use fastrapi_routing::route::RouteEntry;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use simd_json::json;
use simd_json::owned::Value as JsonValue;
use simd_json::serde::to_owned_value;
use std::collections::HashMap;
use tracing::debug;

pub type RouteCollector = fn(Python<'_>, &Bound<'_, PyAny>) -> Option<Vec<RouteEntry>>;

pub struct AppDocument<'a> {
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
) -> JsonValue {
    let mut spec = OpenApiSpec::default();

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

    if let Some(servers) = app.servers {
        spec.servers = py_dicts_to_json_vec(py, servers);
    }

    if app.root_path_in_servers && !app.root_path.is_empty() {
        spec.servers.get_or_insert_with(Vec::new).push(json!({
            "url": app.root_path
        }));
    }

    if let Some(tags) = app.openapi_tags {
        spec.tags = py_dicts_to_json_vec(py, tags);
    }

    if let Some(docs) = app.openapi_external_docs
        && let Ok(dict) = docs.bind(py).cast::<PyDict>()
    {
        spec.external_docs = Some(py_dict_to_json(py, dict));
    }

    let app_responses = if let Some(resp) = app.responses
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
        app.routes,
        &mut schemas,
        app_responses.as_ref(),
        app.separate_input_output_schemas,
        app.generate_unique_id_function,
        &mut security_schemes,
    );

    if let Some(webhooks) = app.webhooks
        && let Some(wh_collected) = collect(py, webhooks.bind(py))
    {
        let wh_paths = build_paths_from_routes(
            py,
            &wh_collected,
            &mut schemas,
            None,
            app.separate_input_output_schemas,
            app.generate_unique_id_function,
            &mut security_schemes,
        );
        spec.webhooks = Some(wh_paths);
    }

    if let Some(components) = &mut spec.components {
        components.schemas = schemas;
        components.security_schemes = security_schemes;
    }
    debug!("Built OpenAPI spec with {} paths", spec.paths.len());
    to_owned_value(&spec).unwrap_or_else(|_| json!({}))
}
