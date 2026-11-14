//! The native `fastrapi.routing` Python module, with the behavior of starlette and FastAPI.
//!
//! Path matching uses the canonical parameter parser of the crate

use std::str::FromStr;

use pyo3::exceptions::PyValueError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{
    PyAny, PyAnyMethods, PyDict, PyDictMethods, PyList, PyModule, PySet, PyString, PyTuple,
};

use fastrapi_core::method::HttpMethod;
use fastrapi_core::path::extract_path_param_names;
use fastrapi_pyutil::json::json_to_py_object;
use fastrapi_pyutil::lazy::LazyPyAttr;

fastrapi_pyutil::cached_py_import!(COMPILE_PATH, "starlette.routing", "compile_path");
fastrapi_pyutil::cached_py_import!(
    DEFAULT_PLACEHOLDER,
    "fastrapi.datastructures",
    "DefaultPlaceholder"
);
fastrapi_pyutil::cached_py_import!(GENERATE_UNIQUE_ID, "fastrapi.utils", "generate_unique_id");
fastrapi_pyutil::cached_py_import!(JSON_RESPONSE, "fastrapi.responses", "JSONResponse");
use fastrapi_routing::route::{RouteEntry, WebSocketEntry};
use pyo3::Bound;

/// Gets the path parameters from a matched request path, in declaration order.
fn path_captures<'py>(
    path: &str,
    request_path: &str,
    py: Python<'py>,
) -> Option<Bound<'py, PyDict>> {
    let segments = path.split('/').zip(request_path.split('/'));
    let captures = PyDict::new(py);
    for (route_segment, request_segment) in segments {
        let Some(placeholder) = route_segment
            .strip_prefix('{')
            .and_then(|s| s.strip_suffix('}'))
        else {
            if route_segment != request_segment {
                return None;
            }
            continue;
        };
        let name = placeholder.split(':').next().unwrap_or(placeholder);
        if request_segment.is_empty() && !matches!(placeholder.split_once(':'), Some((_, "path"))) {
            return None;
        }
        captures.set_item(name, request_segment).ok()?;
    }
    Some(captures)
}

/// Matches a request path and method against a route, as `starlette.routing.Route.matches` does.
/// The function returns `(Match, child_scope)`. It reads the attributes of the route at each call.
/// Thus Python subclasses that change these attributes in `__init__` get the updated behavior.
pub(crate) fn route_matches<'py>(
    route: &Bound<'py, PyAny>,
    scope: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = scope.py();
    let path: String = route.getattr("path")?.extract()?;
    let request_path: String = scope
        .get_item("path")?
        .extract()
        .unwrap_or_else(|_| "/".into());
    let Some(path_params) = path_captures(&path, &request_path, py) else {
        return match_result(py, "NONE");
    };

    let scope_type: Option<String> = scope.get_item("type").ok().and_then(|t| t.extract().ok());
    let methods: Vec<String> = match route.getattr("methods") {
        Ok(methods) if !methods.is_none() => methods
            .try_iter()?
            .filter_map(Result::ok)
            .filter_map(|m| m.extract().ok())
            .collect(),
        _ => Vec::new(),
    };

    let full = match scope_type.as_deref() {
        Some("http") => {
            methods.is_empty()
                || scope
                    .get_item("method")
                    .ok()
                    .and_then(|m| m.extract::<String>().ok())
                    .is_some_and(|request_method| {
                        methods
                            .iter()
                            .any(|m| m.eq_ignore_ascii_case(&request_method))
                    })
        }
        Some("websocket") => true,
        _ => false,
    };
    if !full {
        return match_result(py, "PARTIAL");
    }

    let child_scope = PyDict::new(py);
    child_scope.set_item("path_params", path_params)?;
    if let Ok(endpoint) = route.getattr("endpoint") {
        child_scope.set_item("endpoint", endpoint)?;
    }
    child_scope.set_item("route", route)?;
    let full = match_enum(py)?.getattr("FULL")?;
    Ok(PyTuple::new(py, [full, child_scope.into_any()])?.into_any())
}

/// Returns the `Match` enum of starlette, which is also available as `fastrapi.routing.Match`.
/// Thus `is` checks and annotations in user code have the same behavior.
fn match_enum(py: Python<'_>) -> PyResult<&Bound<'_, PyAny>> {
    static MATCH_ENUM: LazyPyAttr = LazyPyAttr::new("starlette.routing", "Match");
    MATCH_ENUM.get(py)
}

/// Returns the `(member, child_scope)` pair that the `matches` method of starlette returns.
fn match_result<'py>(py: Python<'py>, member: &str) -> PyResult<Bound<'py, PyAny>> {
    let matched = match_enum(py)?.getattr(member)?;
    Ok(PyTuple::new(py, [matched, PyDict::new(py).into_any()])?.into_any())
}

/// Validates a `methods` iterable against the canonical `HttpMethod` set of the crate.
/// For example, starlette gives `["GET"]`.
fn validate_http_methods(methods: &Bound<'_, PyAny>) -> PyResult<()> {
    for method in methods.try_iter()? {
        let method: String = method?.extract()?;
        if HttpMethod::from_str(&method.to_ascii_uppercase()).is_err() {
            return Err(PyValueError::new_err(format!(
                "Invalid HTTP method: {method}"
            )));
        }
    }
    Ok(())
}

#[pyclass(name = "APIRoute", module = "fastrapi.routing", subclass, dict)]
pub struct PyAPIRoute {
    /// The route path with `{param}` placeholders.
    /// `#[pyo3(get, set)]` lets Python read and change it, as the FastAPI `APIRoute` contract requires.
    #[pyo3(get, set)]
    pub path: String,
}

#[pymethods]
impl PyAPIRoute {
    /// Creates the route object, as the `Route.__init__` method of starlette does.
    /// `tp_new` and `tp_init` receive all the arguments. Thus Python subclasses that call
    /// `super().__init__(path, endpoint, **kwargs)` work the same as with FastAPI.
    /// `__new__` keeps the subtype allocation of pyo3, and `__init__` sets the state.
    #[new]
    #[pyo3(signature = (*args, **kwargs))]
    const fn py_new(
        #[expect(unused_variables, reason = "`__init__` reads the arguments")] args: &Bound<
            '_,
            PyTuple,
        >,
        #[expect(unused_variables, reason = "`__init__` reads the arguments")] kwargs: Option<
            &Bound<'_, PyDict>,
        >,
    ) -> Self {
        Self {
            path: String::new(),
        }
    }

    #[pyo3(signature = (path, endpoint=None, *, methods=None, name=None, include_in_schema=true, **kwargs))]
    fn __init__(
        slf: &Bound<'_, Self>,
        path: &str,
        endpoint: Option<&Bound<'_, PyAny>>,
        methods: Option<&Bound<'_, PyAny>>,
        name: Option<&Bound<'_, PyAny>>,
        include_in_schema: bool,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        let py = slf.py();
        let methods = match methods {
            Some(methods) if !methods.is_none() => {
                validate_http_methods(methods)?;
                let upper = PySet::empty(slf.py())?;
                for method in methods.try_iter()? {
                    upper.add(method?.call_method0(intern!(slf.py(), "upper"))?)?;
                }
                upper.into_any().unbind()
            }
            _ => PySet::new(slf.py(), ["GET"])?.into_any().unbind(),
        };
        let default_name: Py<PyAny> = match name {
            Some(name) if !name.is_none() => name.clone().unbind(),
            _ => match endpoint.and_then(|endpoint| endpoint.getattr("__name__").ok()) {
                Some(endpoint_name) => endpoint_name.unbind(),
                None => path.into_pyobject(slf.py())?.unbind().into(),
            },
        };
        let attrs = PyDict::new(slf.py());
        attrs.set_item("endpoint", endpoint)?;
        attrs.set_item("methods", &methods)?;
        attrs.set_item("name", default_name)?;
        attrs.set_item("include_in_schema", include_in_schema)?;
        let (path_regex, path_format, param_convertors) = COMPILE_PATH
            .get(py)?
            .call1((path,))?
            .extract::<(Bound<'_, PyAny>, Bound<'_, PyAny>, Bound<'_, PyAny>)>()?;
        attrs.set_item("path_regex", path_regex)?;
        attrs.set_item("path_format", path_format)?;
        attrs.set_item("param_convertors", param_convertors)?;
        let placeholder = DEFAULT_PLACEHOLDER.get(py)?;
        attrs.set_item(
            "response_class",
            placeholder.call1((JSON_RESPONSE.get(py)?,))?,
        )?;
        attrs.set_item(
            "generate_unique_id_function",
            placeholder.call1((GENERATE_UNIQUE_ID.get(py)?,))?,
        )?;
        attrs.set_item("strict_content_type", placeholder.call1((true,))?)?;
        attrs.set_item("response_model_by_alias", true)?;
        for key in [
            "response_model_exclude_unset",
            "response_model_exclude_defaults",
            "response_model_exclude_none",
        ] {
            attrs.set_item(key, false)?;
        }
        for key in [
            "summary",
            "operation_id",
            "deprecated",
            "response_model",
            "status_code",
            "openapi_extra",
            "callbacks",
            "response_model_include",
            "response_model_exclude",
            "dependency_overrides_provider",
        ] {
            attrs.set_item(key, py.None())?;
        }
        attrs.set_item("tags", PyList::empty(slf.py()))?;
        attrs.set_item("dependencies", PyList::empty(slf.py()))?;
        attrs.set_item("responses", PyDict::new(slf.py()))?;
        attrs.set_item("response_description", "Successful Response")?;
        let description = endpoint
            .and_then(|endpoint| endpoint.getattr("__doc__").ok())
            .filter(|doc| !doc.is_none())
            .map(|doc| {
                slf.py()
                    .import("inspect")?
                    .call_method1("cleandoc", (doc,))?
                    .call_method1("split", ("\x0c",))?
                    .get_item(0)
            })
            .transpose()?
            .filter(|doc| doc.is_truthy().unwrap_or(false));
        attrs.set_item("description", description)?;
        attrs.set_item(
            "param_names",
            extract_path_param_names(path).into_pyobject(slf.py())?,
        )?;
        if let Some(kwargs) = kwargs {
            attrs.update(kwargs.as_mapping())?;
        }
        slf.borrow_mut().path = path.to_string();
        let instance_dict = slf.as_any().getattr("__dict__")?;
        instance_dict.call_method1("update", (&attrs,))?;
        let operation_id = attrs.get_item("operation_id")?.filter(|id| !id.is_none());
        let unique_id = if let Some(operation_id) = operation_id {
            operation_id
        } else {
            let generate = match attrs
                .get_item("generate_unique_id_function")?
                .filter(|function| !function.is_none())
            {
                Some(function) if function.is_instance(placeholder)? => {
                    function.getattr(intern!(py, "value"))?
                }
                Some(function) => function,
                None => GENERATE_UNIQUE_ID.get(py)?.clone(),
            };
            generate.call1((slf,))?
        };
        slf.setattr(intern!(py, "unique_id"), unique_id)
    }

    fn matches<'py>(
        slf: &Bound<'py, Self>,
        scope: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        route_matches(slf.as_any(), scope)
    }

    #[pyo3(signature = (name, /, **path_params))]
    fn url_path_for<'py>(
        slf: &Bound<'py, Self>,
        py: Python<'py>,
        name: &str,
        path_params: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let route = py.import("starlette.routing")?.getattr("Route")?;
        let endpoint = slf.getattr("endpoint")?.unbind();
        let kwargs = PyDict::new(py);
        kwargs.set_item("name", slf.getattr("name")?)?;
        let delegate = route.call((slf.borrow().path.clone(), endpoint), Some(&kwargs))?;
        if let Some(params) = path_params {
            delegate.call_method("url_path_for", (name,), Some(params))
        } else {
            delegate.call_method1("url_path_for", (name,))
        }
    }

    #[getter]
    fn dependant<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        cached_route_value(slf, intern!(slf.py(), "_dependant"), "route_dependant")
    }

    #[getter]
    fn body_field<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        cached_route_value(slf, intern!(slf.py(), "_body_field"), "route_body_field")
    }

    #[getter]
    fn response_field<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        cached_route_value(
            slf,
            intern!(slf.py(), "_response_field"),
            "route_response_field",
        )
    }

    #[getter]
    fn app<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        slf.as_any()
            .getattr(intern!(slf.py(), "dependency_overrides_provider"))
    }

    fn get_route_handler(slf: &Bound<'_, Self>, py: Python<'_>) -> Py<PyAny> {
        slf.as_any()
            .getattr("_fastrapi_original")
            .or_else(|_| slf.as_any().getattr("endpoint"))
            .map_or_else(|_| py.None(), Bound::unbind)
    }

    fn handle<'py>(
        slf: &Bound<'py, Self>,
        py: Python<'py>,
        scope: Py<PyAny>,
        receive: Py<PyAny>,
        send: Py<PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let route = py.import("starlette.routing")?.getattr("Route")?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("methods", slf.getattr("methods")?)?;
        kwargs.set_item("name", slf.getattr("name")?)?;
        let delegate = route.call(
            (slf.borrow().path.clone(), slf.getattr("endpoint")?),
            Some(&kwargs),
        )?;
        delegate.call_method1("handle", (scope, receive, send))
    }

    fn __repr__(slf: &Bound<'_, Self>) -> String {
        format!("APIRoute(path={})", slf.borrow().path)
    }
}

#[pyclass(frozen, name = "RouteEntry", module = "fastrapi.routing")]
pub struct PyRouteEntry {
    pub entry: RouteEntry,
}

pub(crate) fn route_objects<'py>(
    py: Python<'py>,
    routes: &[RouteEntry],
    websockets: &[WebSocketEntry],
) -> PyResult<Bound<'py, PyList>> {
    let objects = PyList::empty(py);
    for entry in routes {
        if let Some(existing) = entry.route_object.lock().as_ref() {
            objects.append(existing.bind(py))?;
            continue;
        }
        let kwargs = PyDict::new(py);
        kwargs.set_item("methods", vec![entry.method.as_ref()])?;
        kwargs.set_item("name", &entry.name)?;
        kwargs.set_item("include_in_schema", entry.include_in_schema)?;
        kwargs.set_item("tags", &entry.tags)?;
        kwargs.set_item("summary", &entry.summary)?;
        kwargs.set_item(
            "description",
            entry.description.as_deref().unwrap_or_default(),
        )?;
        kwargs.set_item(
            "response_description",
            entry
                .response_description
                .as_deref()
                .unwrap_or("Successful Response"),
        )?;
        kwargs.set_item("operation_id", &entry.operation_id)?;
        kwargs.set_item("deprecated", entry.deprecated)?;
        match &entry.responses {
            Some(responses) => kwargs.set_item("responses", responses)?,
            None => kwargs.set_item("responses", PyDict::new(py))?,
        }
        let markers = match &entry.dependency_markers {
            Some(markers) => py.get_type::<PyList>().call1((markers,))?,
            None => PyList::empty(py).into_any(),
        };
        kwargs.set_item("dependencies", markers)?;
        let callbacks = match &entry.callback_routes {
            Some(routes) => py.get_type::<PyList>().call1((routes,))?,
            None => PyList::empty(py).into_any(),
        };
        kwargs.set_item("callbacks", callbacks)?;
        kwargs.set_item(
            "response_model",
            entry.handler.response.response_model.as_ref(),
        )?;
        kwargs.set_item(
            "status_code",
            entry
                .handler
                .response
                .default_status
                .map(|status| status.as_u16()),
        )?;
        kwargs.set_item(
            "openapi_extra",
            entry
                .openapi_extra
                .as_ref()
                .map(|extra| json_to_py_object(py, extra)),
        )?;
        let response = &entry.handler.response;
        if entry.explicit_response_class {
            kwargs.set_item("response_class", response.response_class.as_ref())?;
        }
        if let Some(function) = &entry.generate_unique_id_function {
            kwargs.set_item("generate_unique_id_function", function)?;
        }
        if let Some(strict) = entry.handler.payload.strict_content_type {
            kwargs.set_item("strict_content_type", strict)?;
        }
        let options = response
            .dump_options
            .as_ref()
            .map(|options| options.bind(py));
        let option = |key: &str| -> PyResult<Option<Bound<'py, PyAny>>> {
            options.map_or(Ok(None), |options| options.get_item(key))
        };
        kwargs.set_item("response_model_include", option("include")?)?;
        kwargs.set_item("response_model_exclude", option("exclude")?)?;
        for (key, name) in [
            ("by_alias", "response_model_by_alias"),
            ("exclude_unset", "response_model_exclude_unset"),
            ("exclude_defaults", "response_model_exclude_defaults"),
            ("exclude_none", "response_model_exclude_none"),
        ] {
            kwargs.set_item(
                name,
                option(key)?.is_some_and(|value| value.is_truthy().unwrap_or(false)),
            )?;
        }
        kwargs.set_item(
            "_fastrapi_entry",
            Py::new(
                py,
                PyRouteEntry {
                    entry: entry.clone(),
                },
            )?,
        )?;
        let route = py.get_type::<PyAPIRoute>().call(
            (entry.path.as_str(), entry.handler.execution.func.bind(py)),
            Some(&kwargs),
        )?;
        *entry.route_object.lock() = Some(route.clone().unbind());
        objects.append(route)?;
    }
    for websocket in websockets {
        let kwargs = PyDict::new(py);
        kwargs.set_item("name", &websocket.name)?;
        let route = py.get_type::<PyAPIWebSocketRoute>().call(
            (websocket.path.as_str(), websocket.handler.bind(py)),
            Some(&kwargs),
        )?;
        objects.append(route)?;
    }
    Ok(objects)
}

#[pyclass(
    name = "APIWebSocketRoute",
    module = "fastrapi.routing",
    subclass,
    dict
)]
pub struct PyAPIWebSocketRoute {
    #[pyo3(get, set)]
    pub path: String,
}

#[pymethods]
impl PyAPIWebSocketRoute {
    #[new]
    #[pyo3(signature = (*args, **kwargs))]
    const fn new(
        #[expect(unused_variables, reason = "`__init__` reads the arguments")] args: &Bound<
            '_,
            PyTuple,
        >,
        #[expect(unused_variables, reason = "`__init__` reads the arguments")] kwargs: Option<
            &Bound<'_, PyDict>,
        >,
    ) -> Self {
        Self {
            path: String::new(),
        }
    }

    #[pyo3(signature = (path, endpoint=None, *, name=None, **kwargs))]
    fn __init__(
        slf: &Bound<'_, Self>,
        path: &str,
        endpoint: Option<&Bound<'_, PyAny>>,
        name: Option<&Bound<'_, PyAny>>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        let py = slf.py();
        path.clone_into(&mut slf.borrow_mut().path);
        let attrs = PyDict::new(py);
        attrs.set_item("endpoint", endpoint)?;
        attrs.set_item("name", name.unwrap_or(&path.into_pyobject(py)?.into_any()))?;
        if let Some(kwargs) = kwargs {
            attrs.update(kwargs.as_mapping())?;
        }
        slf.as_any()
            .getattr("__dict__")?
            .call_method1("update", (attrs,))?;
        Ok(())
    }

    fn matches<'py>(
        slf: &Bound<'py, Self>,
        py: Python<'py>,
        scope: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let route = py.import("starlette.routing")?.getattr("WebSocketRoute")?;
        let delegate =
            route.call1((slf.borrow().path.clone(), slf.as_any().getattr("endpoint")?))?;
        delegate.call_method1("matches", (scope,))
    }

    #[pyo3(signature = (name, /, **params))]
    fn url_path_for<'py>(
        slf: &Bound<'py, Self>,
        py: Python<'py>,
        name: &str,
        params: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let route = py.import("starlette.routing")?.getattr("WebSocketRoute")?;
        let delegate = route.call(
            (slf.borrow().path.clone(), slf.as_any().getattr("endpoint")?),
            Some(&PyDict::new(py)),
        )?;
        if let Some(params) = params {
            delegate.call_method("url_path_for", (name,), Some(params))
        } else {
            delegate.call_method1("url_path_for", (name,))
        }
    }

    fn handle<'py>(
        slf: &Bound<'py, Self>,
        py: Python<'py>,
        scope: Py<PyAny>,
        receive: Py<PyAny>,
        send: Py<PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let route = py.import("starlette.routing")?.getattr("WebSocketRoute")?;
        let delegate =
            route.call1((slf.borrow().path.clone(), slf.as_any().getattr("endpoint")?))?;
        delegate.call_method1("handle", (scope, receive, send))
    }
}

/// RouteContext(route: starlette.routing.BaseRoute, _route_context: fastapi.routing._EffectiveRouteContext | None = None)
#[pyclass(name = "RouteContext", module = "fastrapi.routing")]
pub struct PyRouteContext {
    #[pyo3(get)]
    pub route: Py<PyAny>,
    original_route: Py<PyAny>,
}

#[pymethods]
impl PyRouteContext {
    #[new]
    #[pyo3(signature = (route, original_route=None))]
    fn new(route: Py<PyAny>, original_route: Option<Py<PyAny>>) -> Self {
        Self {
            original_route: original_route.unwrap_or_else(|| route.clone()),
            route,
        }
    }

    #[getter]
    fn original_route(&self) -> Py<PyAny> {
        self.original_route.clone()
    }

    fn __getattr__(slf: &Bound<'_, Self>, name: &str) -> PyResult<Py<PyAny>> {
        Ok(slf.getattr("route")?.getattr(name)?.unbind())
    }
}

#[pyfunction]
#[pyo3(signature = (routes))]
pub fn iter_route_contexts(py: Python<'_>, routes: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    let contexts = PyList::empty(py);
    for route in routes.try_iter()? {
        let route = route?;
        if route.cast::<PyRouteContext>().is_ok() {
            contexts.append(route)?;
            continue;
        }
        contexts.append(Bound::new(py, PyRouteContext::new(route.unbind(), None))?)?;
    }
    Ok(py
        .import("builtins")?
        .getattr("iter")?
        .call1((contexts,))?
        .unbind())
}

fn cached_route_value<'py>(
    route: &Bound<'py, PyAPIRoute>,
    slot: &Bound<'py, PyString>,
    builder: &str,
) -> PyResult<Bound<'py, PyAny>> {
    let py = route.py();
    let instance = route.as_any().getattr(intern!(py, "__dict__"))?;
    let instance = instance.cast::<PyDict>()?;
    if let Some(value) = instance.get_item(slot)? {
        return Ok(value);
    }
    let value = crate::ffi::compat::py_dependant(py)?
        .getattr(builder)?
        .call1((route,))?;
    instance.set_item(slot, &value)?;
    Ok(value)
}

/// Adds the classes and functions of the `fastrapi.routing` module to `module`.
pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyAPIRoute>()?;
    module.add_class::<PyAPIWebSocketRoute>()?;
    module.add_class::<PyRouteContext>()?;
    module.add_function(wrap_pyfunction!(iter_route_contexts, module)?)?;
    let py = module.py();
    let match_enum = match_enum(py)?;
    module.setattr("Match", match_enum)?;
    for name in ["NONE", "PARTIAL", "FULL"] {
        module.setattr(name, match_enum.getattr(name)?)?;
    }
    Ok(())
}
