use fastrapi_core::path::extract_path_param_names;
use fastrapi_http::responses::{PyHTMLResponse, PyJSONResponse};
use fastrapi_routing::dependencies::parse_dependencies;
use pyo3::Bound;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyModule, PyTuple};

fn python_type<'py>(
    py: Python<'py>,
    name: &str,
    bases: &Bound<'py, PyTuple>,
) -> PyResult<Bound<'py, PyAny>> {
    py.import("builtins")?
        .getattr("type")?
        .call1((name, bases, PyDict::new(py)))
}

fn str_type(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    py.import("builtins")?.getattr("str")
}

pub(crate) fn register_security(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    let base = PyModule::new(py, "base")?;
    let object = py.import("builtins")?.getattr("object")?;
    let bases = PyTuple::new(py, [object])?;
    for name in ["SecurityBase", "APIKeyBase", "HTTPBase"] {
        base.add(name, python_type(py, name, &bases)?)?;
    }
    parent.add_submodule(&base)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("fastrapi.security.base", &base)?;
    Ok(())
}

#[pyfunction]
#[pyo3(signature = (call, *, path="/", path_param_names=None, security_scopes=None))]
fn get_dependant(
    py: Python<'_>,
    call: &Bound<'_, PyAny>,
    path: &str,
    path_param_names: Option<Vec<String>>,
    security_scopes: Option<Vec<String>>,
) -> PyResult<Py<PyAny>> {
    let names = path_param_names.unwrap_or_default();
    let plan = parse_dependencies(py, call, &names)?;
    let dependencies = PyList::empty(py);
    let namespace = py.import("types")?.getattr("SimpleNamespace")?;
    for node in plan {
        let kwargs = PyDict::new(py);
        kwargs.set_item("call", node.func.bind(py))?;
        kwargs.set_item("use_cache", node.use_cache)?;
        kwargs.set_item("security_scopes", node.scopes)?;
        dependencies.append(namespace.call((), Some(&kwargs))?)?;
    }
    let kwargs = PyDict::new(py);
    kwargs.set_item("call", call)?;
    kwargs.set_item("path", path)?;
    kwargs.set_item("security_scopes", security_scopes.unwrap_or_default())?;
    kwargs.set_item("dependencies", dependencies)?;
    Ok(namespace.call((), Some(&kwargs))?.unbind())
}

#[pyfunction]
fn get_path_param_names(path: &str) -> Vec<String> {
    extract_path_param_names(path)
}

#[pyfunction]
#[pyo3(signature = (*args, **_kwargs))]
fn get_flat_dependant(
    args: &Bound<'_, PyTuple>,
    _kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyAny>> {
    args.get_item(0).map(Bound::unbind)
}

#[pyfunction]
#[pyo3(signature = (route))]
fn generate_unique_id(py: Python<'_>, route: &Bound<'_, PyAny>) -> String {
    let name = route
        .getattr("name")
        .ok()
        .and_then(|v| v.extract().ok())
        .unwrap_or_else(|| "route".to_owned());
    let path = route
        .getattr("path_format")
        .or_else(|_| route.getattr("path"))
        .ok()
        .and_then(|v| v.extract().ok())
        .unwrap_or_else(|| "/".to_owned());
    let method = route
        .getattr("methods")
        .ok()
        .and_then(|v| v.try_iter().ok())
        .and_then(|mut it| it.next().transpose().ok().flatten())
        .and_then(|v| v.extract().ok())
        .unwrap_or_else(|| "get".to_owned());
    let _ = py;
    format!(
        "{}_{}_{}",
        name,
        path.replace('/', "_").replace(['{', '}'], ""),
        method.to_ascii_lowercase()
    )
}

#[pyfunction]
#[pyo3(signature = (first_item, *extra_items))]
fn get_value_or_default(
    py: Python<'_>,
    first_item: Py<PyAny>,
    extra_items: &Bound<'_, PyTuple>,
) -> Py<PyAny> {
    if !first_item.bind(py).is_none() {
        return first_item;
    }
    for item in extra_items.iter() {
        if !item.is_none() {
            return item.unbind();
        }
    }
    py.None()
}

pub(crate) fn register_dependencies(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    parent.add_function(wrap_pyfunction!(get_dependant, parent)?)?;
    parent.add_function(wrap_pyfunction!(get_path_param_names, parent)?)?;
    let utils = PyModule::new(py, "utils")?;
    utils.add("get_dependant", parent.getattr("get_dependant")?)?;
    utils.add(
        "get_path_param_names",
        parent.getattr("get_path_param_names")?,
    )?;
    utils.add_function(wrap_pyfunction!(get_flat_dependant, &utils)?)?;
    parent.add_submodule(&utils)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("fastrapi.dependencies", parent)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("fastrapi.dependencies.utils", &utils)?;
    let models = PyModule::new(py, "models")?;
    let object = py.import("builtins")?.getattr("object")?;
    let bases = PyTuple::new(py, [object])?;
    models.add("Dependant", python_type(py, "Dependant", &bases)?)?;
    models.add(
        "SecurityRequirement",
        python_type(py, "SecurityRequirement", &bases)?,
    )?;
    parent.add_submodule(&models)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("fastrapi.dependencies.models", &models)?;
    Ok(())
}

#[pyfunction]
fn http_exception_handler(
    py: Python<'_>,
    _request: &Bound<'_, PyAny>,
    exc: &Bound<'_, PyAny>,
) -> PyResult<Py<PyAny>> {
    let content = PyDict::new(py);
    content.set_item("detail", exc.getattr("detail")?)?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("status_code", exc.getattr("status_code")?)?;
    if let Ok(headers) = exc.getattr("headers")
        && !headers.is_none()
    {
        kwargs.set_item("headers", headers)?;
    }
    Ok(py
        .get_type::<PyJSONResponse>()
        .call((content,), Some(&kwargs))?
        .unbind())
}

#[pyfunction]
fn request_validation_exception_handler(
    py: Python<'_>,
    _request: &Bound<'_, PyAny>,
    exc: &Bound<'_, PyAny>,
) -> PyResult<Py<PyAny>> {
    let content = PyDict::new(py);
    content.set_item("detail", exc.call_method0("errors")?)?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("status_code", 422)?;
    Ok(py
        .get_type::<PyJSONResponse>()
        .call((content,), Some(&kwargs))?
        .unbind())
}

#[pyfunction]
fn websocket_request_validation_exception_handler(
    websocket: &Bound<'_, PyAny>,
    exc: &Bound<'_, PyAny>,
) -> PyResult<Py<PyAny>> {
    let kwargs = PyDict::new(websocket.py());
    kwargs.set_item("code", 1008)?;
    kwargs.set_item("reason", exc.call_method0("errors")?.str()?)?;
    Ok(websocket.call_method("close", (), Some(&kwargs))?.unbind())
}

pub(crate) fn register_exception_handlers(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    parent.add_function(wrap_pyfunction!(http_exception_handler, parent)?)?;
    parent.add_function(wrap_pyfunction!(
        request_validation_exception_handler,
        parent
    )?)?;
    parent.add_function(wrap_pyfunction!(
        websocket_request_validation_exception_handler,
        parent
    )?)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("fastrapi.exception_handlers", parent)?;
    Ok(())
}

pub(crate) fn register_openapi_helpers(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = module.py();
    let docs = PyModule::new(py, "docs")?;
    docs.add_function(wrap_pyfunction!(swagger_ui_html, &docs)?)?;
    docs.add_function(wrap_pyfunction!(redoc_html, &docs)?)?;
    docs.add_function(wrap_pyfunction!(swagger_ui_oauth2_redirect_html, &docs)?)?;
    module.add_submodule(&docs)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("fastrapi.openapi.docs", &docs)?;
    let models = PyModule::new(py, "models")?;
    let pydantic = py.import("pydantic")?;
    let config_kwargs = PyDict::new(py);
    config_kwargs.set_item("extra", "allow")?;
    let config = pydantic
        .getattr("ConfigDict")?
        .call((), Some(&config_kwargs))?;
    let typing = py.import("typing")?;
    let any = typing.getattr("Any")?;
    let optional = typing.getattr("Optional")?;
    let list = typing.getattr("List")?;
    let dict = typing.getattr("Dict")?;
    let ellipsis = py.import("builtins")?.getattr("Ellipsis")?;
    let optional_any = optional.get_item(any)?;
    let make = |name: &str, fields: Vec<(&str, Py<PyAny>)>| -> PyResult<Py<PyAny>> {
        let kwargs = PyDict::new(py);
        kwargs.set_item("__config__", &config)?;
        for (field, value) in fields {
            kwargs.set_item(field, value)?;
        }
        Ok(pydantic
            .getattr("create_model")?
            .call((name,), Some(&kwargs))?
            .unbind())
    };
    let field =
        |annotation: &Bound<'_, PyAny>, default: &Bound<'_, PyAny>| -> PyResult<Py<PyAny>> {
            Ok(PyTuple::new(py, [annotation.clone(), default.clone()])?
                .into_any()
                .unbind())
        };
    let none = py.None().into_bound(py);
    let empty = PyList::empty(py).into_any();
    let empty_dict = PyDict::new(py).into_any();
    let contact = make(
        "Contact",
        vec![
            ("name", field(&optional_any, &none)?),
            ("url", field(&optional_any, &none)?),
            ("email", field(&optional_any, &none)?),
        ],
    )?;
    let license = make(
        "License",
        vec![
            ("name", field(&str_type(py)?, &ellipsis)?),
            ("identifier", field(&optional_any, &none)?),
            ("url", field(&optional_any, &none)?),
        ],
    )?;
    let info = make(
        "Info",
        vec![
            ("title", field(&str_type(py)?, &ellipsis)?),
            ("summary", field(&optional_any, &none)?),
            ("description", field(&optional_any, &none)?),
            ("termsOfService", field(&optional_any, &none)?),
            (
                "contact",
                field(&optional.get_item(contact.bind(py))?, &none)?,
            ),
            (
                "license",
                field(&optional.get_item(license.bind(py))?, &none)?,
            ),
            ("version", field(&str_type(py)?, &ellipsis)?),
        ],
    )?;
    let server_variable = make(
        "ServerVariable",
        vec![
            ("enum", field(&list.get_item(str_type(py)?)?, &empty)?),
            ("default", field(&str_type(py)?, &ellipsis)?),
            ("description", field(&optional_any, &none)?),
        ],
    )?;
    let server = make(
        "Server",
        vec![
            ("url", field(&str_type(py)?, &ellipsis)?),
            ("description", field(&optional_any, &none)?),
            (
                "variables",
                field(
                    &dict.get_item((str_type(py)?, server_variable.bind(py)))?,
                    &empty_dict,
                )?,
            ),
        ],
    )?;
    let external_docs = make(
        "ExternalDocumentation",
        vec![
            ("description", field(&optional_any, &none)?),
            ("url", field(&str_type(py)?, &ellipsis)?),
        ],
    )?;
    let tag = make(
        "Tag",
        vec![
            ("name", field(&str_type(py)?, &ellipsis)?),
            ("description", field(&optional_any, &none)?),
            (
                "externalDocs",
                field(&optional.get_item(external_docs.bind(py))?, &none)?,
            ),
        ],
    )?;
    let components = make(
        "Components",
        vec![
            ("schemas", field(&dict, &empty_dict)?),
            ("responses", field(&dict, &empty_dict)?),
            ("parameters", field(&dict, &empty_dict)?),
            ("examples", field(&dict, &empty_dict)?),
            ("requestBodies", field(&dict, &empty_dict)?),
            ("headers", field(&dict, &empty_dict)?),
            ("securitySchemes", field(&dict, &empty_dict)?),
            ("links", field(&dict, &empty_dict)?),
            ("callbacks", field(&dict, &empty_dict)?),
            ("pathItems", field(&dict, &empty_dict)?),
        ],
    )?;
    let info_any = info.bind(py).clone().into_any();
    let openapi = make(
        "OpenAPI",
        vec![
            ("openapi", field(&str_type(py)?, &ellipsis)?),
            ("info", field(&info_any, &ellipsis)?),
            ("servers", field(&list.get_item(server.bind(py))?, &empty)?),
            ("paths", field(&dict, &empty_dict)?),
            (
                "components",
                field(&optional.get_item(components.bind(py))?, &none)?,
            ),
            ("security", field(&optional_any, &none)?),
            ("tags", field(&optional.get_item(tag.bind(py))?, &none)?),
            (
                "externalDocs",
                field(&optional.get_item(external_docs.bind(py))?, &none)?,
            ),
        ],
    )?;
    for (name, value) in [
        ("OpenAPI", openapi),
        ("Info", info),
        ("Contact", contact),
        ("License", license),
        ("Server", server),
        ("ServerVariable", server_variable),
        ("ExternalDocumentation", external_docs),
        ("Tag", tag),
        ("Components", components),
    ] {
        models.add(name, value.bind(py))?;
    }
    let path_item = make(
        "PathItem",
        vec![
            ("summary", field(&optional_any, &none)?),
            ("description", field(&optional_any, &none)?),
            ("get", field(&optional_any, &none)?),
            ("put", field(&optional_any, &none)?),
            ("post", field(&optional_any, &none)?),
            ("delete", field(&optional_any, &none)?),
            ("options", field(&optional_any, &none)?),
            ("head", field(&optional_any, &none)?),
            ("patch", field(&optional_any, &none)?),
            ("trace", field(&optional_any, &none)?),
            ("servers", field(&list, &empty)?),
            ("parameters", field(&list, &empty)?),
        ],
    )?;
    let operation = make(
        "Operation",
        vec![
            ("tags", field(&list, &empty)?),
            ("summary", field(&optional_any, &none)?),
            ("description", field(&optional_any, &none)?),
            ("operationId", field(&optional_any, &none)?),
            ("parameters", field(&list, &empty)?),
            ("requestBody", field(&optional_any, &none)?),
            ("responses", field(&dict, &empty_dict)?),
            ("callbacks", field(&dict, &empty_dict)?),
            ("deprecated", field(&optional_any, &none)?),
            ("security", field(&optional_any, &none)?),
            ("servers", field(&list, &empty)?),
        ],
    )?;
    let parameter = make(
        "Parameter",
        vec![
            ("name", field(&str_type(py)?, &ellipsis)?),
            ("in", field(&str_type(py)?, &ellipsis)?),
            ("description", field(&optional_any, &none)?),
            ("required", field(&optional_any, &none)?),
            ("deprecated", field(&optional_any, &none)?),
            ("schema", field(&optional_any, &none)?),
            ("example", field(&optional_any, &none)?),
            ("examples", field(&dict, &empty_dict)?),
        ],
    )?;
    let request_body = make(
        "RequestBody",
        vec![
            ("description", field(&optional_any, &none)?),
            ("content", field(&dict, &ellipsis)?),
            ("required", field(&optional_any, &none)?),
        ],
    )?;
    let response = make(
        "Response",
        vec![
            ("description", field(&str_type(py)?, &ellipsis)?),
            ("headers", field(&dict, &empty_dict)?),
            ("content", field(&dict, &empty_dict)?),
            ("links", field(&dict, &empty_dict)?),
        ],
    )?;
    let security_scheme = make(
        "SecurityScheme",
        vec![
            ("type", field(&str_type(py)?, &ellipsis)?),
            ("description", field(&optional_any, &none)?),
            ("name", field(&optional_any, &none)?),
            ("in", field(&optional_any, &none)?),
            ("scheme", field(&optional_any, &none)?),
            ("bearerFormat", field(&optional_any, &none)?),
            ("flows", field(&optional_any, &none)?),
            ("openIdConnectUrl", field(&optional_any, &none)?),
        ],
    )?;
    for (name, value) in [
        ("PathItem", path_item),
        ("Operation", operation),
        ("Parameter", parameter),
        ("RequestBody", request_body),
        ("Response", response),
        ("SecurityScheme", security_scheme),
    ] {
        models.add(name, value.bind(py))?;
    }
    module.add_submodule(&models)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("fastrapi.openapi.models", &models)?;
    Ok(())
}

#[pyfunction]
fn generate_operation_id_for_path(name: &str, path: &str, method: &str) -> String {
    format!(
        "{}_{}_{}",
        name,
        path.replace('/', "_"),
        method.to_ascii_lowercase()
    )
}

#[pyfunction]
fn deep_dict_update(main: &Bound<'_, PyDict>, update: &Bound<'_, PyDict>) -> PyResult<Py<PyDict>> {
    for (key, value) in update.iter() {
        if let Some(existing) = main.get_item(&key)?
            && let (Ok(left), Ok(right)) = (existing.cast::<PyDict>(), value.cast::<PyDict>())
        {
            deep_dict_update(left, right)?;
            continue;
        }
        main.set_item(key, value)?;
    }
    Ok(main.clone().unbind())
}

#[pyfunction]
#[pyo3(signature = (status_code))]
fn is_body_allowed_for_status_code(status_code: Option<&Bound<'_, PyAny>>) -> bool {
    let Some(status_code) = status_code else {
        return true;
    };
    let Ok(status) = status_code.extract::<u16>() else {
        return true;
    };
    !(100..200).contains(&status) && status != 204 && status != 304
}

pub(crate) fn register_compat_modules(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    let concurrency = PyModule::new(py, "concurrency")?;
    let starlette = py.import("starlette.concurrency")?;
    for name in [
        "run_in_threadpool",
        "iterate_in_threadpool",
        "run_until_first_complete",
        "contextmanager_in_threadpool",
    ] {
        if let Ok(value) = starlette.getattr(name) {
            concurrency.add(name, value)?;
        }
    }
    parent.add_submodule(&concurrency)?;

    let utils = PyModule::new(py, "utils")?;
    utils.add_function(wrap_pyfunction!(generate_operation_id_for_path, &utils)?)?;
    utils.add_function(wrap_pyfunction!(deep_dict_update, &utils)?)?;
    utils.add_function(wrap_pyfunction!(is_body_allowed_for_status_code, &utils)?)?;
    utils.add_function(wrap_pyfunction!(get_path_param_names, &utils)?)?;
    utils.add_function(wrap_pyfunction!(generate_unique_id, &utils)?)?;
    utils.add_function(wrap_pyfunction!(get_value_or_default, &utils)?)?;
    parent.add_submodule(&utils)?;

    let params = PyModule::new(py, "param_functions")?;
    for name in [
        "Body", "Cookie", "Depends", "File", "Form", "Header", "Path", "Query", "Security",
    ] {
        if let Ok(value) = parent.getattr(name) {
            params.add(name, value)?;
        }
    }
    parent.add_submodule(&params)?;
    for (name, module) in [
        ("fastrapi.concurrency", &concurrency),
        ("fastrapi.utils", &utils),
        ("fastrapi.param_functions", &params),
    ] {
        py.import("sys")?
            .getattr("modules")?
            .set_item(name, module)?;
    }
    Ok(())
}

fn html_response(py: Python<'_>, title: &str) -> PyResult<Py<PyAny>> {
    Ok(py
        .get_type::<PyHTMLResponse>()
        .call1((format!(
            "<!doctype html><html><head><title>{title}</title></head><body></body></html>"
        ),))?
        .unbind())
}

#[pyfunction(name = "get_swagger_ui_html")]
#[pyo3(signature = (*, openapi_url, title, **_kwargs))]
fn swagger_ui_html(
    py: Python<'_>,
    openapi_url: &str,
    title: &str,
    _kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyAny>> {
    html_response(py, &format!("{title} - Swagger UI ({openapi_url})"))
}

#[pyfunction(name = "get_redoc_html")]
#[pyo3(signature = (*, openapi_url, title, **_kwargs))]
fn redoc_html(
    py: Python<'_>,
    openapi_url: &str,
    title: &str,
    _kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyAny>> {
    html_response(py, &format!("{title} - ReDoc ({openapi_url})"))
}

#[pyfunction(name = "get_swagger_ui_oauth2_redirect_html")]
#[pyo3(signature = (*_args, **_kwargs))]
fn swagger_ui_oauth2_redirect_html(
    py: Python<'_>,
    _args: &Bound<'_, PyTuple>,
    _kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyAny>> {
    html_response(py, "OAuth2 Redirect")
}
