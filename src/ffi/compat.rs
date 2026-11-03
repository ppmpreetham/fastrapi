//! Compatibility functions that copy the Python API of FastAPI.
//! This module builds the `fastrapi` submodules for dependencies, security, exception handlers, OpenAPI and docs.

use fastrapi_core::path::extract_path_param_names;
use fastrapi_http::exceptions::PyFastAPIDeprecationWarning;
use fastrapi_http::responses::PyJSONResponse;
use fastrapi_routing::dependencies::parse_dependencies;
use pyo3::Bound;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyModule, PyTuple};
use std::collections::HashSet;

const PY_HELPERS: &std::ffi::CStr = c"import abc
import datetime
import re
from collections import defaultdict, deque
from contextlib import AsyncExitStack, asynccontextmanager
from decimal import Decimal
from enum import Enum
from ipaddress import (
    IPv4Address,
    IPv4Interface,
    IPv4Network,
    IPv6Address,
    IPv6Interface,
    IPv6Network,
)
from pathlib import Path
from types import GeneratorType
from uuid import UUID

import anyio
import anyio.to_thread
from pydantic import AnyUrl, NameEmail, SecretBytes, SecretStr
from pydantic_core import Url
from starlette.concurrency import run_in_threadpool


def isoformat(o):
    return o.isoformat()


def decimal_encoder(dec_value):
    exponent = dec_value.as_tuple().exponent
    if isinstance(exponent, int) and exponent >= 0:
        return int(dec_value)
    return float(dec_value)


ENCODERS_BY_TYPE = {
    bytes: lambda o: o.decode(),
    datetime.date: isoformat,
    datetime.datetime: isoformat,
    datetime.time: isoformat,
    datetime.timedelta: lambda td: td.total_seconds(),
    Decimal: decimal_encoder,
    Enum: lambda o: o.value,
    frozenset: list,
    deque: list,
    GeneratorType: list,
    IPv4Address: str,
    IPv4Interface: str,
    IPv4Network: str,
    IPv6Address: str,
    IPv6Interface: str,
    IPv6Network: str,
    NameEmail: str,
    Path: str,
    re.Pattern: lambda o: o.pattern,
    SecretBytes: str,
    SecretStr: str,
    set: list,
    UUID: str,
    Url: str,
    AnyUrl: str,
}


def generate_encoders_by_class_tuples(type_encoder_map):
    encoders_by_class_tuples = defaultdict(tuple)
    for type_, encoder in type_encoder_map.items():
        encoders_by_class_tuples[encoder] += (type_,)
    return encoders_by_class_tuples


encoders_by_class_tuples = generate_encoders_by_class_tuples(ENCODERS_BY_TYPE)


class Param(abc.ABC):
    pass


def _async_lifespan(factory):
    import inspect
    from contextlib import contextmanager

    if inspect.isasyncgenfunction(factory):
        return asynccontextmanager(factory)
    if inspect.isgeneratorfunction(factory):
        sync_factory = contextmanager(factory)

        @asynccontextmanager
        async def wrapped(app):
            with sync_factory(app) as state:
                yield state

        return wrapped
    return factory


def merge_lifespan_context(original, nested):
    original = _async_lifespan(original)
    nested = _async_lifespan(nested)

    @asynccontextmanager
    async def merged_lifespan(app):
        async with original(app) as maybe_original_state:
            async with nested(app) as maybe_nested_state:
                if maybe_nested_state is None and maybe_original_state is None:
                    yield None
                else:
                    yield {**(maybe_nested_state or {}), **(maybe_original_state or {})}

    return merged_lifespan


def default_lifespan(router):
    import inspect

    @asynccontextmanager
    async def lifespan(app):
        for handler in router.on_startup:
            result = handler()
            if inspect.isawaitable(result):
                await result
        yield None
        for handler in router.on_shutdown:
            result = handler()
            if inspect.isawaitable(result):
                await result

    return lifespan


def request_endpoint(endpoint):
    import inspect

    from fastrapi import Request

    if inspect.iscoroutinefunction(endpoint):

        async def route(request: Request):
            return await endpoint(request)

    else:

        def route(request: Request):
            return endpoint(request)

    route.__name__ = getattr(endpoint, \"__name__\", \"route\")
    route.__doc__ = endpoint.__doc__
    return route


class SecurityBase(abc.ABC):
    pass


class APIKeyBase(SecurityBase):
    pass


class HTTPBase(SecurityBase):
    pass


class AsyncExitStackMiddleware:
    def __init__(self, app, context_name=\"fastapi_middleware_astack\"):
        self.app = app
        self.context_name = context_name

    async def __call__(self, scope, receive, send):
        async with AsyncExitStack() as stack:
            scope[self.context_name] = stack
            await self.app(scope, receive, send)


@asynccontextmanager
async def contextmanager_in_threadpool(cm):
    exit_limiter = anyio.CapacityLimiter(1)
    try:
        yield await run_in_threadpool(cm.__enter__)
    except Exception as e:
        ok = bool(
            await anyio.to_thread.run_sync(
                cm.__exit__, type(e), e, e.__traceback__, limiter=exit_limiter
            )
        )
        if not ok:
            raise e
    else:
        await anyio.to_thread.run_sync(
            cm.__exit__, None, None, None, limiter=exit_limiter
        )
";

static PY_HELPERS_MODULE: pyo3::sync::PyOnceLock<Py<PyModule>> = pyo3::sync::PyOnceLock::new();

pub(crate) fn py_helpers(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    PY_HELPERS_MODULE
        .get_or_try_init(py, || {
            PyModule::from_code(
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
fn generate_unique_id(_py: Python<'_>, route: &Bound<'_, PyAny>) -> String {
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
    format!(
        "{}_{}",
        word_characters(&format!("{name}{path}")),
        method.to_ascii_lowercase()
    )
}

fn word_characters(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
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
    module.add_class::<DefaultPlaceholder>()?;
    module.add_function(wrap_pyfunction!(default_placeholder, module)?)
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

pub(crate) fn python_source_module<'py>(
    py: Python<'py>,
    source: &str,
    file_name: &std::ffi::CStr,
    module_name: &std::ffi::CStr,
) -> PyResult<Bound<'py, PyModule>> {
    let source = std::ffi::CString::new(source)
        .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
    PyModule::from_code(py, &source, file_name, module_name)
}

pub(crate) fn register_openapi_helpers(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = module.py();
    let docs = PyModule::new(py, "docs")?;
    crate::ffi::docs::register(&docs)?;
    module.add_submodule(&docs)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("fastrapi.openapi.docs", &docs)?;
    let models = python_source_module(
        py,
        include_str!("py/openapi_models.py"),
        c"fastrapi/openapi/models.py",
        c"fastrapi.openapi.models",
    )?;
    module.add_submodule(&models)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("fastrapi.openapi.models", &models)?;
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
        &py.get_type::<PyFastAPIDeprecationWarning>(),
        c"fastapi.utils.generate_operation_id_for_path() was deprecated, it is not used internally, and will be removed soon",
        2,
    )?;
    Ok(format!(
        "{}_{}",
        word_characters(&format!("{name}{path}")),
        method.to_ascii_lowercase()
    ))
}

#[pyfunction]
fn deep_dict_update(main: &Bound<'_, PyDict>, update: &Bound<'_, PyDict>) -> PyResult<()> {
    for (key, value) in update.iter() {
        if let Some(existing) = main.get_item(&key)? {
            if let (Ok(left), Ok(right)) = (existing.cast::<PyDict>(), value.cast::<PyDict>()) {
                deep_dict_update(left, right)?;
                continue;
            }
            if existing.is_instance_of::<PyList>() && value.is_instance_of::<PyList>() {
                main.set_item(key, existing.add(value)?)?;
                continue;
            }
        }
        main.set_item(key, value)?;
    }
    Ok(())
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
        py.import("sys")?
            .getattr("modules")?
            .set_item(name, module)?;
    }
    Ok(())
}
