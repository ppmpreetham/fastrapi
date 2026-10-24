//! The native `fastrapi.openapi` Python module: `constants` and `utils.get_openapi`.
//!
//! `get_openapi` builds a full schema from route objects. A route object can be any
//! object with `path`, `methods` and `endpoint` attributes, for example a native `APIRoute` subclass.
//! The function sends these routes through the router code of the app.
//! Thus the output is the same as the served `/openapi.json`, and only one implementation exists.

use crate::ffi::compat::register_openapi_helpers;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};

/// The values of `fastapi.openapi.constants`.
pub(crate) const REF_TEMPLATE: &str = "#/components/schemas/{model}";
pub(crate) const METHODS_WITH_BODY: [&str; 6] = ["GET", "HEAD", "POST", "PUT", "DELETE", "PATCH"];

// This function is the same as `fastapi.openapi.utils.get_openapi`.
//
// `routes` can contain `APIRoute` instances, `RouteContext` objects, or other
// objects that have `path`, `methods` and `endpoint` attributes.
// FastAPI accepts the same duck-typed objects.
#[pyo3::pyfunction]
#[pyo3(signature = (*, title, version, openapi_version="3.1.0", summary=None, description=None,
    routes, webhooks=None, tags=None, servers=None, terms_of_service=None, contact=None,
    license_info=None, separate_input_output_schemas=true, external_docs=None))]
#[allow(clippy::too_many_arguments)]
pub fn get_openapi(
    py: Python<'_>,
    title: String,
    version: String,
    openapi_version: &str,
    summary: Option<&str>,
    description: Option<&str>,
    routes: &Bound<'_, PyAny>,
    webhooks: Option<&Bound<'_, PyAny>>,
    tags: Option<&Bound<'_, PyAny>>,
    servers: Option<&Bound<'_, PyAny>>,
    terms_of_service: Option<&str>,
    contact: Option<&Bound<'_, PyAny>>,
    license_info: Option<&Bound<'_, PyAny>>,
    separate_input_output_schemas: bool,
    external_docs: Option<&Bound<'_, PyAny>>,
) -> PyResult<Py<PyAny>> {
    let info = PyDict::new(py);
    info.set_item("title", title)?;
    info.set_item("version", version)?;
    if let Some(summary) = summary {
        info.set_item("summary", summary)?;
    }
    if let Some(description) = description {
        info.set_item("description", description)?;
    }
    if let Some(terms) = terms_of_service {
        info.set_item("termsOfService", terms)?;
    }
    if let Some(contact) = contact {
        info.set_item("contact", contact)?;
    }
    if let Some(license_info) = license_info {
        info.set_item("license", license_info)?;
    }

    let output = PyDict::new(py);
    output.set_item("openapi", openapi_version)?;
    output.set_item("info", &info)?;
    if let Some(servers) = servers {
        output.set_item("servers", servers)?;
    }
    output.set_item(
        "paths",
        paths_from_routes(py, routes, separate_input_output_schemas)?,
    )?;
    if let Some(webhooks) = webhooks.filter(|w| !w.is_none() && w.len().is_ok_and(|n| n > 0)) {
        output.set_item(
            "webhooks",
            paths_from_routes(py, webhooks, separate_input_output_schemas)?,
        )?;
    }
    if let Some(tags) = tags {
        output.set_item("tags", tags)?;
    }
    if let Some(external_docs) = external_docs {
        output.set_item("externalDocs", external_docs)?;
    }
    Ok(output.into_any().unbind())
}

/// Builds the `paths` section for a sequence of route objects.
/// The function adds the routes to a temporary `FastrAPI` app and uses the real schema builder.
fn paths_from_routes(
    py: Python<'_>,
    routes: &Bound<'_, PyAny>,
    separate_input_output_schemas: bool,
) -> PyResult<Py<PyAny>> {
    let _ = separate_input_output_schemas;
    let paths = PyDict::new(py);
    for context in routes.try_iter()? {
        let context = context?;
        let route = context.getattr("route").unwrap_or_else(|_| context.clone());
        let Ok(path) = route.getattr("path").and_then(|p| p.extract::<String>()) else {
            continue;
        };
        let Ok(methods) = route.getattr("methods") else {
            continue;
        };
        let Ok(endpoint) = route.getattr("endpoint") else {
            continue;
        };
        let include_in_schema = route
            .getattr("include_in_schema")
            .and_then(|v| v.extract::<bool>())
            .unwrap_or(true);
        if !include_in_schema {
            continue;
        }

        let entry = match paths.get_item(&path)? {
            Some(entry) => entry,
            None => PyDict::new(py).into_any(),
        };
        let entry = entry.cast_into::<PyDict>()?;
        let method_list: Vec<String> = methods
            .try_iter()?
            .filter_map(|m| m.ok()?.extract().ok())
            .collect();
        entry.set_item(
            method_list.join(", ").to_lowercase(),
            operation_for(py, &endpoint, &method_list)?,
        )?;
        paths.set_item(&path, &entry)?;
    }
    Ok(paths.into_any().unbind())
}

/// Builds a minimal operation entry for one endpoint.
fn operation_for(
    py: Python<'_>,
    endpoint: &Bound<'_, PyAny>,
    methods: &[String],
) -> PyResult<Py<PyAny>> {
    let operation = PyDict::new(py);
    let summary = endpoint
        .getattr("__doc__")
        .ok()
        .and_then(|doc| doc.extract::<String>().ok())
        .filter(|s| !s.trim().is_empty());
    if let Some(summary) = summary {
        operation.set_item("summary", summary.trim())?;
    }
    let name = endpoint
        .getattr("__name__")
        .and_then(|n| n.extract::<String>())
        .unwrap_or_else(|_| "endpoint".to_string());
    let joined = methods.join("_").to_lowercase();
    operation.set_item("operationId", format!("{name}_{joined}_path"))?;
    operation.set_item("responses", PyDict::new(py))?;
    Ok(operation.into_any().unbind())
}

/// Adds the `fastrapi.openapi` module (`constants` and `utils`) to the parent module.
pub(crate) fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    let openapi_module = PyModule::new(py, "openapi")?;

    let constants = PyModule::new(py, "constants")?;
    constants.add("REF_TEMPLATE", REF_TEMPLATE)?;
    constants.add("METHODS_WITH_BODY", METHODS_WITH_BODY)?;
    openapi_module.add_submodule(&constants)?;

    let utils = PyModule::new(py, "utils")?;
    utils.add_function(pyo3::wrap_pyfunction!(get_openapi, &utils)?)?;
    openapi_module.add_submodule(&utils)?;
    parent.add_submodule(&openapi_module)?;

    for (name, module) in [
        ("fastrapi.openapi", &openapi_module),
        ("fastrapi.openapi.constants", &constants),
        ("fastrapi.openapi.utils", &utils),
    ] {
        py.import("sys")?
            .getattr("modules")?
            .set_item(name, module)?;
    }
    register_openapi_helpers(&openapi_module)?;
    Ok(())
}
