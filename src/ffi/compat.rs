//! Compatibility functions that copy the Python API of FastAPI.
//! This module builds the `fastrapi` submodules for dependencies, security, exception handlers, OpenAPI and docs.

use fastrapi_core::path::extract_path_param_names;
use fastrapi_http::exceptions::deprecation_warning_type;
use fastrapi_openapi::schema::native_operation_id;
pub(crate) use fastrapi_pyutil::imports::python_source_module;
use fastrapi_routing::dependencies::parse_dependencies;
use pyo3::Bound;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyModule, PyTuple};
use std::collections::HashSet;

const PY_HELPERS: &str = include_str!("py/helpers.py");

static PY_HELPERS_MODULE: pyo3::sync::PyOnceLock<Py<PyModule>> = pyo3::sync::PyOnceLock::new();

pub(crate) fn py_helpers(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    PY_HELPERS_MODULE
        .get_or_try_init(py, || {
            python_source_module(
                py,
                PY_HELPERS,
                c"fastrapi/_helpers.py",
                c"fastrapi._helpers",
            )
            .map(Bound::unbind)
        })
        .map(|module| module.bind(py).clone())
}

fn python_type<'py>(
    py: Python<'py>,
    name: &str,
    bases: &Bound<'py, PyTuple>,
) -> PyResult<Bound<'py, PyAny>> {
    py.import("builtins")?
        .getattr("type")?
        .call1((name, bases, PyDict::new(py)))
}

pub(crate) fn register_security(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    let base = PyModule::new(py, "base")?;
    let helpers = py_helpers(py)?;
    for name in ["SecurityBase", "APIKeyBase", "HTTPBase"] {
        let class = helpers.getattr(name)?;
        class.setattr("__module__", "fastrapi.security.base")?;
        base.add(name, class)?;
    }
    for (submodule, name, classes) in [
        (
            "api_key",
            "APIKeyBase",
            &["APIKeyQuery", "APIKeyHeader", "APIKeyCookie"][..],
        ),
        (
            "http",
            "HTTPBase",
            &["HTTPBasic", "HTTPBearer", "HTTPDigest"][..],
        ),
        (
            "oauth2",
            "SecurityBase",
            &[
                "OAuth2",
                "OAuth2PasswordBearer",
                "OAuth2AuthorizationCodeBearer",
            ][..],
        ),
        (
            "open_id_connect_url",
            "SecurityBase",
            &["OpenIdConnect"][..],
        ),
    ] {
        let Ok(module) = parent.getattr(submodule) else {
            continue;
        };
        let base_class = base.getattr(name)?;
        if name != "SecurityBase" {
            module.setattr(name, &base_class)?;
        }
        for class in classes {
            if let Ok(class) = module.getattr(*class) {
                base_class.call_method1("register", (class,))?;
            }
        }
    }
    parent.add_submodule(&base)?;
    fastrapi_pyutil::imports::register_module(py, "fastrapi.security.base", &base)?;
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
        kwargs.set_item("security_scopes", node.scopes.to_vec())?;
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
fn get_path_param_names(path: &str) -> HashSet<String> {
    extract_path_param_names(path).into_iter().collect()
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
fn generate_unique_id(route: &Bound<'_, PyAny>) -> String {
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
    native_operation_id(&name, &path, &method)
}

#[pyclass(
    name = "DefaultPlaceholder",
    module = "fastrapi.datastructures",
    frozen
)]
pub(crate) struct DefaultPlaceholder {
    #[pyo3(get)]
    value: Py<PyAny>,
}

#[pymethods]
impl DefaultPlaceholder {
    #[new]
    const fn new(value: Py<PyAny>) -> Self {
        Self { value }
    }

    fn __bool__(&self, py: Python<'_>) -> PyResult<bool> {
        self.value.bind(py).is_truthy()
    }

    fn __eq__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<bool> {
        match other.cast::<Self>() {
            Ok(other) => self.value.bind(py).eq(other.get().value.bind(py)),
            Err(_) => Ok(false),
        }
    }
}

#[pyfunction(name = "Default")]
const fn default_placeholder(value: Py<PyAny>) -> DefaultPlaceholder {
    DefaultPlaceholder::new(value)
}

pub(crate) fn register_default_placeholder(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = module.py();
    module.add_class::<DefaultPlaceholder>()?;
    module.add_function(wrap_pyfunction!(default_placeholder, module)?)?;
    let default_type = py
        .import("typing")?
        .getattr("TypeVar")?
        .call1(("DefaultType",))?;
    module.add("DefaultType", default_type)
}

/// Pass items or `DefaultPlaceholder`s by descending priority.
///
/// The first one to _not_ be a `DefaultPlaceholder` will be returned.
///
/// Otherwise, the first item (a `DefaultPlaceholder`) will be returned.
#[pyfunction]
#[pyo3(signature = (first_item, *extra_items))]
fn get_value_or_default(
    py: Python<'_>,
    first_item: Py<PyAny>,
    extra_items: &Bound<'_, PyTuple>,
) -> Py<PyAny> {
    if !first_item.bind(py).is_instance_of::<DefaultPlaceholder>() {
        return first_item;
    }
    extra_items
        .iter()
        .find(|item| !item.is_instance_of::<DefaultPlaceholder>())
        .map_or(first_item, Bound::unbind)
}

pub(crate) fn register_types(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = module.py();
    let builtins = py.import("builtins")?;
    let typing = py.import("typing")?;
    let any = typing.getattr("Any")?;
    let callable = py
        .import("collections.abc")?
        .getattr("Callable")?
        .get_item((builtins.getattr("Ellipsis")?, &any))?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("bound", &callable)?;
    module.add(
        "DecoratedCallable",
        typing
            .getattr("TypeVar")?
            .call(("DecoratedCallable",), Some(&kwargs))?,
    )?;
    module.add("IncEx", py.import("pydantic.main")?.getattr("IncEx")?)?;
    module.add("UnionType", py.import("types")?.getattr("UnionType")?)?;
    let type_of = builtins.getattr("type")?;
    let model_or_enum = type_of
        .get_item(py.import("pydantic")?.getattr("BaseModel")?)?
        .bitor(type_of.get_item(py.import("enum")?.getattr("Enum")?)?)?;
    let str_type = builtins.getattr("str")?;
    module.add(
        "ModelNameMap",
        builtins
            .getattr("dict")?
            .get_item((model_or_enum, &str_type))?,
    )?;
    let tuple = builtins.getattr("tuple")?;
    module.add(
        "DependencyCacheKey",
        tuple.get_item((
            callable.bitor(py.None())?,
            tuple.get_item((&str_type, builtins.getattr("Ellipsis")?))?,
            &str_type,
        ))?,
    )
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
    let params = fastrapi_routing::params::params_module(py)?;
    for name in [
        "analyze_param",
        "get_typed_signature",
        "get_typed_annotation",
        "ParamDetails",
    ] {
        utils.add(name, params.getattr(name)?)?;
    }
    parent.add_submodule(&utils)?;
    fastrapi_pyutil::imports::register_module(py, "fastrapi.dependencies", parent)?;
    fastrapi_pyutil::imports::register_module(py, "fastrapi.dependencies.utils", &utils)?;
    let models = PyModule::new(py, "models")?;
    let object = py.import("builtins")?.getattr("object")?;
    let bases = PyTuple::new(py, [object])?;
    models.add("Dependant", python_type(py, "Dependant", &bases)?)?;
    models.add(
        "SecurityRequirement",
        python_type(py, "SecurityRequirement", &bases)?,
    )?;
    parent.add_submodule(&models)?;
    fastrapi_pyutil::imports::register_module(py, "fastrapi.dependencies.models", &models)?;
    Ok(())
}

pub(crate) fn register_openapi_helpers(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = module.py();
    let docs = PyModule::new(py, "docs")?;
    crate::ffi::docs::register(&docs)?;
    module.add_submodule(&docs)?;
    fastrapi_pyutil::imports::register_module(py, "fastrapi.openapi.docs", &docs)?;
    let models = python_source_module(
        py,
        include_str!("py/openapi_models.py"),
        c"fastrapi/openapi/models.py",
        c"fastrapi.openapi.models",
    )?;
    module.add_submodule(&models)?;
    fastrapi_pyutil::imports::register_module(py, "fastrapi.openapi.models", &models)?;
    Ok(())
}

#[pyfunction]
#[pyo3(signature = (*, name, path, method))]
fn generate_operation_id_for_path(
    py: Python<'_>,
    name: &str,
    path: &str,
    method: &str,
) -> PyResult<String> {
    PyErr::warn(
        py,
        deprecation_warning_type(py)?,
        c"fastapi.utils.generate_operation_id_for_path() was deprecated, it is not used internally, and will be removed soon",
        2,
    )?;
    Ok(native_operation_id(name, path, method))
}

#[pyfunction]
fn deep_dict_update(main_dict: &Bound<'_, PyDict>, update_dict: &Bound<'_, PyDict>) -> PyResult<()> {
    for (key, value) in update_dict.iter() {
        if let Some(existing) = main_dict.get_item(&key)? {
            if let (Ok(left), Ok(right)) = (existing.cast::<PyDict>(), value.cast::<PyDict>()) {
                deep_dict_update(left, right)?;
                continue;
            }
            if existing.is_instance_of::<PyList>() && value.is_instance_of::<PyList>() {
                main_dict.set_item(key, existing.add(value)?)?;
                continue;
            }
        }
        main_dict.set_item(key, value)?;
    }
    Ok(())
}

#[pyfunction]
#[pyo3(signature = (status_code))]
fn is_body_allowed_for_status_code(status_code: Option<&Bound<'_, PyAny>>) -> PyResult<bool> {
    let Some(status_code) = status_code.filter(|code| !code.is_none()) else {
        return Ok(true);
    };
    if status_code
        .extract::<&str>()
        .is_ok_and(|code| matches!(code, "default" | "1XX" | "2XX" | "3XX" | "4XX" | "5XX"))
    {
        return Ok(true);
    }
    let status: i64 = status_code
        .py()
        .get_type::<pyo3::types::PyInt>()
        .call1((status_code,))?
        .extract()?;
    Ok(u16::try_from(status).map_or(status >= 200, fastrapi_http::status::body_allowed))
}

pub(crate) fn register_compat_modules(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    let concurrency = PyModule::new(py, "concurrency")?;
    let starlette = py.import("starlette.concurrency")?;
    for name in [
        "run_in_threadpool",
        "iterate_in_threadpool",
        "run_until_first_complete",
    ] {
        if let Ok(value) = starlette.getattr(name) {
            concurrency.add(name, value)?;
        }
    }
    let contextlib = py.import("contextlib")?;
    for name in ["asynccontextmanager", "AsyncExitStack"] {
        concurrency.add(name, contextlib.getattr(name)?)?;
    }
    let helpers = py_helpers(py)?;
    concurrency.add(
        "contextmanager_in_threadpool",
        helpers.getattr("contextmanager_in_threadpool")?,
    )?;
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
        fastrapi_pyutil::imports::register_module(py, name, module)?;
    }
    Ok(())
}
