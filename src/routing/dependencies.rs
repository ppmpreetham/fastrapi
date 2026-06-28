use super::params;
use super::security::PySecurityScopes;
use super::types::{ParsedParameter, RequestInput};
use crate::ffi::pydantic;
use axum::response::Response;
use once_cell::sync::OnceCell;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule, PyString, PyTuple};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::sync::Arc;

type SharedPyObject = Py<PyAny>;

static INSPECT_MODULE: OnceCell<Py<PyModule>> = OnceCell::new();

fn get_inspect(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    INSPECT_MODULE
        .get_or_try_init(|| py.import(intern!(py, "inspect")).map(Bound::unbind))
        .map(|module| module.bind(py).clone())
}

struct ParserKeys<'py> {
    parameters: &'py Bound<'py, PyString>,
    items: &'py Bound<'py, PyString>,
    default: &'py Bound<'py, PyString>,
    dependency: &'py Bound<'py, PyString>,
    annotation: &'py Bound<'py, PyString>,
    scopes: &'py Bound<'py, PyString>,
    use_cache: &'py Bound<'py, PyString>,
}

impl<'py> ParserKeys<'py> {
    fn new(py: Python<'py>) -> Self {
        Self {
            parameters: intern!(py, "parameters"),
            items: intern!(py, "items"),
            default: intern!(py, "default"),
            dependency: intern!(py, "dependency"),
            annotation: intern!(py, "annotation"),
            scopes: intern!(py, "scopes"),
            use_cache: intern!(py, "use_cache"),
        }
    }
}

#[derive(Clone, Debug)]
pub enum InjectionType {
    Dependency(usize),
    Parameter(ParsedParameter),
    Request,
    SecurityScopes,
}

#[derive(Clone, Debug)]
pub struct DependencyNode {
    pub func_id: u64,
    pub func: Py<PyAny>,
    pub is_async: bool,
    pub param_name: Option<String>,
    pub scopes: Vec<String>,
    pub use_cache: bool,
    pub is_top_level: bool,
    pub injection_plan: Vec<(Py<PyString>, InjectionType)>,
    pub needs_request_object: bool,
}

pub enum DependencyExecutionError {
    Python(PyErr),
    Response(Response),
}

impl From<PyErr> for DependencyExecutionError {
    fn from(value: PyErr) -> Self {
        Self::Python(value)
    }
}

fn get_signature<'py>(
    py: Python<'py>,
    func: &Bound<'py, PyAny>,
    inspect: &Bound<'py, PyModule>,
) -> PyResult<Bound<'py, PyAny>> {
    if let Ok(signature) = func.getattr(intern!(py, "__signature__")) {
        return Ok(signature);
    }
    inspect.call_method1(intern!(py, "signature"), (func,))
}

fn is_async_callable(
    py: Python<'_>,
    inspect: &Bound<'_, PyModule>,
    func: &Bound<'_, PyAny>,
) -> bool {
    if let Ok(is_coroutine) = inspect.call_method1(intern!(py, "iscoroutinefunction"), (func,))
        && is_coroutine.is_truthy().unwrap_or(false) {
            return true;
        }
    if let Ok(call_method) = func.getattr(intern!(py, "__call__"))
        && let Ok(is_coroutine) =
            inspect.call_method1(intern!(py, "iscoroutinefunction"), (call_method,))
            && is_coroutine.is_truthy().unwrap_or(false) {
                return true;
            }
    false
}

fn annotation_display_name(py: Python<'_>, annotation: &Bound<'_, PyAny>) -> Option<String> {
    annotation
        .getattr(intern!(py, "__name__"))
        .ok()
        .and_then(|value| {
            value
                .cast::<PyString>()
                .ok()
                .and_then(|name| name.to_str().ok())
                .map(str::to_owned)
        })
        .or_else(|| {
            annotation
                .str()
                .ok()
                .map(|value| value.to_string_lossy().into_owned())
        })
}

fn extract_string_list(value: &Bound<'_, PyAny>) -> Option<Vec<String>> {
    let mut values = Vec::new();
    value.try_iter().ok()?.for_each(|item_res| {
        if let Ok(item) = item_res
            && let Ok(s) = item.extract::<String>() {
                values.push(s);
            }
    });

    Some(values)
}

pub fn parse_dependencies(
    py: Python,
    func: &Bound<PyAny>,
    path_param_names: &[String],
) -> PyResult<Vec<DependencyNode>> {
    let inspect = get_inspect(py)?;
    let keys = ParserKeys::new(py);
    let mut flat_plan = Vec::new();
    let mut visited = HashMap::new();

    extract_and_flatten(
        py,
        &inspect,
        &keys,
        func,
        path_param_names,
        true,
        None,
        Vec::new(),
        true,
        &mut flat_plan,
        &mut visited,
    )?;

    flat_plan.pop();
    Ok(flat_plan)
}

fn extract_and_flatten(
    py: Python,
    inspect: &Bound<'_, PyModule>,
    keys: &ParserKeys<'_>,
    func: &Bound<PyAny>,
    path_param_names: &[String],
    is_top_level: bool,
    parent_param_name: Option<String>,
    scopes: Vec<String>,
    use_cache: bool,
    flat_plan: &mut Vec<DependencyNode>,
    visited: &mut HashMap<u64, usize>,
) -> PyResult<usize> {
    let func_id = func.as_ptr() as u64;

    if use_cache
        && let Some(&idx) = visited.get(&func_id) {
            return Ok(idx);
        }

    let signature = get_signature(py, func, inspect)?;
    let parameters = signature.getattr(keys.parameters)?;

    let mut sub_deps: SmallVec<[(String, usize); 4]> = SmallVec::new();
    let items = parameters.call_method0(keys.items)?;

    for item in items.try_iter()? {
        let pair = item?.cast_into::<PyTuple>()?;
        let param_name = pair.get_item(0)?;
        let param_obj = pair.get_item(1)?;
        let param_name_str = param_name.extract::<String>()?;

        if param_name_str == "self" || param_name_str == "cls" || param_name_str == "return" {
            continue;
        }

        if let Ok(default) = param_obj.getattr(keys.default) {
            if params::is_inspect_empty(py, &default) {
                continue;
            }

            let is_depends = default.hasattr(keys.dependency).unwrap_or(false);
            let is_security = default.hasattr(keys.scopes).unwrap_or(false);

            if !is_depends && !is_security {
                continue;
            }

            let target_callable = if let Ok(dep) = default.getattr(keys.dependency) {
                if dep.is_none() {
                    if let Ok(annotation) = param_obj.getattr(keys.annotation) {
                        annotation
                    } else {
                        continue;
                    }
                } else {
                    dep
                }
            } else {
                continue;
            };

            let child_scopes = if is_security {
                default
                    .getattr(keys.scopes)
                    .ok()
                    .and_then(|value| extract_string_list(&value))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            let child_use_cache = default
                .getattr(keys.use_cache)
                .ok()
                .and_then(|value| value.is_truthy().ok())
                .unwrap_or(true);

            let target_index = extract_and_flatten(
                py,
                inspect,
                keys,
                &target_callable,
                path_param_names,
                is_top_level && parent_param_name.is_none(),
                Some(param_name_str.clone()),
                child_scopes,
                child_use_cache,
                flat_plan,
                visited,
            )?;

            sub_deps.push((param_name_str, target_index));
        }
    }

    let (injection_plan, needs_request_object) =
        build_injection_plan(py, func, path_param_names, &sub_deps, inspect, keys)?;
    let injection_plan = injection_plan
        .into_iter()
        .map(|(name, injection)| (PyString::intern(py, &name).unbind(), injection))
        .collect();
    let is_async = is_async_callable(py, inspect, func);

    let node_index = flat_plan.len();
    flat_plan.push(DependencyNode {
        func_id,
        func: func.as_unbound().clone(),
        is_async,
        param_name: parent_param_name, // Restored Rust String
        scopes,
        use_cache,
        is_top_level,
        injection_plan, // Restored Rust String tuples
        needs_request_object,
    });

    if use_cache {
        visited.insert(func_id, node_index);
    }

    Ok(node_index)
}

fn build_injection_plan(
    py: Python,
    func: &Bound<PyAny>,
    path_param_names: &[String],
    sub_deps: &[(String, usize)],
    inspect: &Bound<'_, PyModule>,
    keys: &ParserKeys<'_>,
) -> PyResult<(Vec<(String, InjectionType)>, bool)> {
    let mut plan = Vec::new();
    let mut needs_request_object = false;
    let signature = get_signature(py, func, inspect)?;
    let parameters_any = signature.getattr(keys.parameters)?;
    let parameters = parameters_any.call_method0(keys.items)?;

    for item in parameters.try_iter()? {
        let pair = item?.cast_into::<PyTuple>()?;
        let key = pair.get_item(0)?;
        let param = pair.get_item(1)?;
        let name: String = key.extract()?;

        if name == "self" || name == "cls" || name == "return" {
            continue;
        }

        if let Some((_, target_idx)) = sub_deps.iter().find(|(param_name, _)| param_name == &name) {
            plan.push((name, InjectionType::Dependency(*target_idx)));
            continue;
        }

        let mut special = false;
        if let Ok(annotation) = param.getattr(keys.annotation)
            && let Some(annotation_name) = annotation_display_name(py, &annotation) {
                if annotation_name.contains("Request") {
                    plan.push((name.clone(), InjectionType::Request));
                    needs_request_object = true;
                    special = true;
                } else if annotation_name.contains("SecurityScopes") {
                    plan.push((name.clone(), InjectionType::SecurityScopes));
                    special = true;
                }
            }

        if !special {
            let parsed_param = params::parse_parameter_spec(py, &name, &param, path_param_names)?;
            plan.push((name, InjectionType::Parameter(parsed_param)));
        }
    }

    Ok((plan, needs_request_object))
}

fn build_dependency_kwargs(
    py: Python<'_>,
    dep: &DependencyNode,
    results_registry: &[Option<SharedPyObject>],
    request_input: &RequestInput<'_>,
    request: Option<&SharedPyObject>,
) -> Result<Py<PyDict>, DependencyExecutionError> {
    let final_kwargs = PyDict::new(py);

    for (arg_name, injection_type) in &dep.injection_plan {
        let py_arg_name = arg_name.bind(py);
        match injection_type {
            InjectionType::Dependency(target_idx) => {
                if let Some(cached_val) = &results_registry[*target_idx] {
                    final_kwargs.set_item(py_arg_name, cached_val.bind(py))?;
                }
            }
            InjectionType::Parameter(parameter) => {
                if let Some(value) = pydantic::resolve_parameter_value(py, parameter, request_input)
                    .map_err(DependencyExecutionError::Response)?
                {
                    final_kwargs.set_item(py_arg_name, value)?;
                }
            }
            InjectionType::Request => {
                if let Some(req) = request {
                    final_kwargs.set_item(py_arg_name, req.bind(py))?;
                }
            }
            InjectionType::SecurityScopes => {
                let py_scopes = Py::new(py, PySecurityScopes::new(Some(dep.scopes.clone())))?;
                final_kwargs.set_item(py_arg_name, py_scopes)?;
            }
        }
    }

    Ok(final_kwargs.unbind())
}

pub fn execute_dependencies_sync(
    py: Python<'_>,
    flat_plan: &[DependencyNode],
    request_input: &RequestInput<'_>,
    request: Option<Py<PyAny>>,
) -> Result<Vec<(String, SharedPyObject)>, DependencyExecutionError> {
    let request = request;
    let mut results_registry: Vec<Option<SharedPyObject>> = vec![None; flat_plan.len()];

    let mut final_results = Vec::with_capacity(
        flat_plan
            .iter()
            .filter(|node| node.is_top_level && node.param_name.is_some())
            .count(),
    );

    for (i, dep) in flat_plan.iter().enumerate() {
        let py_kwargs =
            build_dependency_kwargs(py, dep, &results_registry, request_input, request.as_ref())?;
        let bound_func = dep.func.bind(py);
        let bound_kwargs = py_kwargs.bind(py);
        let result: SharedPyObject = bound_func.call((), Some(bound_kwargs))?.unbind();

        results_registry[i] = Some(result.clone_ref(py));

        if dep.is_top_level
            && let Some(name) = &dep.param_name {
                final_results.push((name.clone(), result));
            }
    }

    Ok(final_results)
}

pub async fn execute_dependencies(
    rt_handle: tokio::runtime::Handle,
    async_loop: &Arc<Py<PyAny>>,
    flat_plan: &[DependencyNode],
    request_input: &RequestInput<'_>,
    request: Option<Py<PyAny>>,
) -> Result<Vec<(String, SharedPyObject)>, DependencyExecutionError> {
    let request = request;
    let mut results_registry: Vec<Option<SharedPyObject>> = vec![None; flat_plan.len()];

    let mut final_results = Vec::with_capacity(
        flat_plan
            .iter()
            .filter(|node| node.is_top_level && node.param_name.is_some())
            .count(),
    );

    for (i, dep) in flat_plan.iter().enumerate() {
        let result: SharedPyObject = if dep.is_async {
            let future = Python::attach(|py| -> Result<_, DependencyExecutionError> {
                let py_kwargs = build_dependency_kwargs(
                    py,
                    dep,
                    &results_registry,
                    request_input,
                    request.as_ref(),
                )?;
                let bound_func = dep.func.bind(py);
                let bound_kwargs = py_kwargs.bind(py);
                let coroutine = bound_func.call((), Some(bound_kwargs))?.unbind();
                let locals = pyo3_async_runtimes::TaskLocals::new(async_loop.bind(py).clone());
                Ok(pyo3_async_runtimes::tokio::scope(locals, async move {
                    let py_future = Python::attach(|py| {
                        pyo3_async_runtimes::tokio::into_future(coroutine.into_bound(py))
                    })?;
                    py_future.await
                }))
            })?;
            future.await.map_err(DependencyExecutionError::Python)?
        } else {
            let py_kwargs = Python::attach(|py| -> Result<_, DependencyExecutionError> {
                build_dependency_kwargs(py, dep, &results_registry, request_input, request.as_ref())
            })?;

            let py_func = dep.func.clone();

            rt_handle
                .spawn_blocking(move || {
                    Python::attach(|py| -> Result<Py<PyAny>, DependencyExecutionError> {
                        let bound_func = py_func.bind(py);
                        let bound_kwargs = py_kwargs.bind(py);
                        Ok(bound_func.call((), Some(bound_kwargs))?.unbind())
                    })
                })
                .await
                .unwrap_or_else(|_| {
                    Err(DependencyExecutionError::Python(Python::attach(|_py| {
                        pyo3::exceptions::PyRuntimeError::new_err("Spawn blocking failed")
                    })))
                })?
        };

        results_registry[i] = Some(result.clone());

        if dep.is_top_level
            && let Some(name) = &dep.param_name
        {
            final_results.push((name.clone(), result));
        }
    }

    Ok(final_results)
}
