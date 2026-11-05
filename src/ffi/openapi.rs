//! The native `fastrapi.openapi` Python module: `constants` and `utils.get_openapi`.
//!
//! `get_openapi` builds a full schema from route objects. A route object can be any
//! object with `path`, `methods` and `endpoint` attributes, for example a native `APIRoute` subclass.
//! The function sends these routes through the router code of the app.
//! Thus the output is the same as the served `/openapi.json`, and only one implementation exists.

use crate::ffi::compat::{python_source_module, register_openapi_helpers};
use crate::ffi::decorators::{PyAPIRouter, collect_router_routes};
use crate::ffi::routing::PyRouteEntry;
use fastrapi_openapi::spec::{AppDocument, build_openapi_spec};
use fastrapi_pyutil::json::json_to_py_object;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule, PySet};

/// The values of `fastapi.openapi.constants`.
pub(crate) const REF_PREFIX: &str = "#/components/schemas/";
pub(crate) const REF_TEMPLATE: &str = "#/components/schemas/{model}";
pub(crate) const METHODS_WITH_BODY: [&str; 6] = ["GET", "HEAD", "POST", "PUT", "DELETE", "PATCH"];

#[pyo3::pyfunction]
#[pyo3(signature = (*, title, version, openapi_version="3.1.0", summary=None, description=None,
    routes, webhooks=None, tags=None, servers=None, terms_of_service=None, contact=None,
    license_info=None, separate_input_output_schemas=true, external_docs=None))]
#[allow(clippy::too_many_arguments)]
pub fn get_openapi(
    py: Python<'_>,
    title: &str,
    version: &str,
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
    let owned = |value: Option<&Bound<'_, PyAny>>| value.map(|value| value.clone().unbind());
    let (tags, servers, contact, license_info, external_docs) = (
        owned(tags),
        owned(servers),
        owned(contact),
        owned(license_info),
        owned(external_docs),
    );
    let route_router = router_from_routes(py, routes)?;
    let routes = route_router.borrow().snapshot(py)?.0;
    let webhook_router = webhooks
        .filter(|webhooks| !webhooks.is_none())
        .map(|webhooks| router_from_routes(py, webhooks))
        .transpose()?
        .map(|router| router.into_any().unbind());
    let document = AppDocument {
        openapi_version,
        title,
        version,
        summary,
        description: description.unwrap_or_default(),
        terms_of_service,
        contact: contact.as_ref(),
        license_info: license_info.as_ref(),
        servers: servers.as_ref(),
        root_path_in_servers: false,
        root_path: "",
        openapi_tags: tags.as_ref(),
        openapi_external_docs: external_docs.as_ref(),
        responses: None,
        webhooks: webhook_router.as_ref(),
        generate_unique_id_function: None,
        separate_input_output_schemas,
        routes: &routes,
    };
    let spec = build_openapi_spec(py, &document, collect_router_routes);
    Ok(json_to_py_object(py, &spec))
}

const ROUTE_OPTIONS: [&str; 15] = [
    "response_model",
    "status_code",
    "tags",
    "dependencies",
    "summary",
    "description",
    "response_description",
    "responses",
    "deprecated",
    "operation_id",
    "include_in_schema",
    "response_class",
    "name",
    "openapi_extra",
    "generate_unique_id_function",
];

fn router_from_routes<'py>(
    py: Python<'py>,
    routes: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAPIRouter>> {
    let router = Bound::new(py, PyAPIRouter::default())?;
    add_route_objects(py, &router.borrow(), routes)?;
    Ok(router)
}

pub(crate) fn add_route_objects(
    py: Python<'_>,
    router: &PyAPIRouter,
    routes: &Bound<'_, PyAny>,
) -> PyResult<()> {
    for item in routes.try_iter()? {
        let item = item?;
        let route = item.getattr("route").unwrap_or_else(|_| item.clone());
        if let Ok(handle) = route.getattr("_fastrapi_entry")
            && let Ok(handle) = handle.cast::<PyRouteEntry>()
        {
            let mut entry = handle.get().entry.clone();
            fastrapi_openapi::paths::apply_route_overrides(&route, &mut entry);
            router.route_entries.lock().push(entry);
            continue;
        }
        let (Ok(path), Ok(endpoint)) = (
            route
                .getattr("path")
                .and_then(|path| path.extract::<String>()),
            route.getattr("endpoint"),
        ) else {
            continue;
        };
        if endpoint.is_none() {
            continue;
        }
        let Some(methods) = route.getattr("methods").ok().filter(|m| !m.is_none()) else {
            let name = route
                .getattr("name")
                .ok()
                .and_then(|name| name.extract::<String>().ok());
            router
                .create_ws_decorator(py, path, name, None)?
                .bind(py)
                .call1((endpoint,))?;
            continue;
        };
        let methods: Vec<String> = methods
            .try_iter()?
            .filter_map(|method| method.ok()?.extract().ok())
            .collect();
        let kwargs = PyDict::new(py);
        for option in ROUTE_OPTIONS {
            if let Ok(value) = route.getattr(option)
                && !value.is_none()
            {
                kwargs.set_item(option, value)?;
            }
        }
        router.add_api_route_impl(py, &path, &endpoint, Some(methods), &kwargs)?;
    }
    Ok(())
}

/// Adds the `fastrapi.openapi` module (`constants` and `utils`) to the parent module.
pub(crate) fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    let openapi_module = PyModule::new(py, "openapi")?;

    let constants = PyModule::new(py, "constants")?;
    constants.add("REF_TEMPLATE", REF_TEMPLATE)?;
    constants.add("METHODS_WITH_BODY", PySet::new(py, METHODS_WITH_BODY)?)?;
    constants.add("REF_PREFIX", REF_PREFIX)?;
    openapi_module.add_submodule(&constants)?;

    let utils = PyModule::new(py, "utils")?;
    utils.add_function(pyo3::wrap_pyfunction!(get_openapi, &utils)?)?;
    let helpers = python_source_module(
        py,
        include_str!("py/openapi_utils.py"),
        c"fastrapi/openapi/utils.py",
        c"fastrapi._openapi_utils",
    )?;
    for name in [
        "validation_error_definition",
        "validation_error_response_definition",
        "status_code_ranges",
        "generate_operation_id",
        "generate_operation_summary",
        "get_openapi_operation_metadata",
        "get_openapi_path",
    ] {
        utils.add(name, helpers.getattr(name)?)?;
    }
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
