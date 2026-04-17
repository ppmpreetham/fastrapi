use super::params;
use super::security::PySecurityScopes;
use super::types::{ParsedParameter, RequestInput};
use crate::ffi::pydantic;
use axum::response::Response;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyString, PyTuple};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

type SharedPyObject = Arc<Py<PyAny>>;

#[derive(Clone, Debug)]
pub enum InjectionType {
    /// Holds the `func_id` of the sub-dependency for O(1) cache lookup.
    Dependency(u64),
    /// A resolved request parameter.
    Parameter(ParsedParameter),
    /// The Request object.
    Request,
    /// Security scopes injected for Security(...).
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
    pub injection_plan: Vec<(String, InjectionType)>,
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

fn get_signature<'py>(py: Python<'py>, func: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    if let Ok(signature) = func.getattr(intern!(py, "__signature__")) {
        return Ok(signature);
    }

    py.import(intern!(py, "inspect"))?
        .call_method1(intern!(py, "signature"), (func,))
}

fn is_async_callable(func: &Bound<'_, PyAny>) -> bool {
    func.getattr("__code__")
        .ok()
        .and_then(|code| code.getattr("co_flags").ok())
        .and_then(|flags| flags.extract::<u32>().ok())
        .map(|flags| flags & 0x80 != 0)
        .unwrap_or(false)
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
    for item in value.try_iter().ok()? {
        let item = item.ok()?;
        values.push(item.extract::<String>().ok()?);
    }
    Some(values)
}

/// Parse dependencies and flatten them into a topologically sorted list.
/// Sub-dependencies always appear before their parents so execution is a
/// simple linear pass at request time.
pub fn parse_dependencies(
    py: Python,
    func: &Bound<PyAny>,
    path_param_names: &[String],
) -> PyResult<Vec<DependencyNode>> {
    let mut flat_plan = Vec::new();
    let mut visited = HashSet::new();
    extract_and_flatten(
        py,
        func,
        path_param_names,
        true,
        None,
        Vec::new(),
        true,
        &mut flat_plan,
        &mut visited,
    )?;
    flat_plan.retain(|node| node.param_name.is_some());
    Ok(flat_plan)
}

fn extract_and_flatten(
    py: Python,
    func: &Bound<PyAny>,
    path_param_names: &[String],
    is_top_level: bool,
    parent_param_name: Option<String>,
    scopes: Vec<String>,
    use_cache: bool,
    flat_plan: &mut Vec<DependencyNode>,
    visited: &mut HashSet<u64>,
) -> PyResult<()> {
    let signature = get_signature(py, func)?;
    let parameters = signature.getattr(intern!(py, "parameters"))?;

    let mut sub_deps: Vec<(String, u64)> = Vec::new();

    let items = parameters.call_method0(intern!(py, "items"))?;
    for item in items.try_iter()? {
        let pair = item?.cast_into::<PyTuple>()?;
        let param_name = pair.get_item(0)?;
        let param_obj = pair.get_item(1)?;
        let param_name_str = param_name.extract::<String>()?;

        if param_name_str == "self" || param_name_str == "cls" || param_name_str == "return" {
            continue;
        }

        if let Ok(default) = param_obj.getattr("default") {
            if params::is_inspect_empty(py, &default) {
                continue;
            }

            let type_name = default.get_type().name()?.to_string();
            if type_name != "Depends" && type_name != "Security" {
                continue;
            }

            let target_callable = if let Ok(dep) = default.getattr("dependency") {
                if dep.is_none() {
                    if let Ok(annotation) = param_obj.getattr("annotation") {
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

            let child_scopes = if type_name == "Security" {
                default
                    .getattr("scopes")
                    .ok()
                    .and_then(|value| extract_string_list(&value))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            let child_use_cache = default
                .getattr("use_cache")
                .ok()
                .and_then(|value| value.is_truthy().ok())
                .unwrap_or(true);

            let target_id = target_callable.as_ptr() as u64;
            sub_deps.push((param_name_str.clone(), target_id));

            if !visited.contains(&target_id) {
                extract_and_flatten(
                    py,
                    &target_callable,
                    path_param_names,
                    is_top_level && parent_param_name.is_none(),
                    Some(param_name_str),
                    child_scopes,
                    child_use_cache,
                    flat_plan,
                    visited,
                )?;
            }
        }
    }

    let func_id = func.as_ptr() as u64;
    if visited.contains(&func_id) {
        return Ok(());
    }

    let (injection_plan, needs_request_object) =
        build_injection_plan(py, func, path_param_names, &sub_deps)?;
    let is_async = is_async_callable(func);

    flat_plan.push(DependencyNode {
        func_id,
        func: func.as_unbound().clone(),
        is_async,
        param_name: parent_param_name,
        scopes,
        use_cache,
        is_top_level,
        injection_plan,
        needs_request_object,
    });
    visited.insert(func_id);

    Ok(())
}

fn build_injection_plan(
    py: Python,
    func: &Bound<PyAny>,
    path_param_names: &[String],
    sub_deps: &[(String, u64)],
) -> PyResult<(Vec<(String, InjectionType)>, bool)> {
    let mut plan = Vec::new();
    let mut needs_request_object = false;
    let signature = get_signature(py, func)?;
    let parameters_any = signature.getattr(intern!(py, "parameters"))?;
    let parameters = parameters_any.call_method0(intern!(py, "items"))?;

    for item in parameters.try_iter()? {
        let pair = item?.cast_into::<PyTuple>()?;
        let key = pair.get_item(0)?;
        let param = pair.get_item(1)?;
        let name: String = key.extract()?;

        if name == "self" || name == "cls" || name == "return" {
            continue;
        }

        if let Some((_, target_id)) = sub_deps.iter().find(|(param_name, _)| param_name == &name) {
            plan.push((name, InjectionType::Dependency(*target_id)));
            continue;
        }

        let mut special = false;
        if let Ok(annotation) = param.getattr("annotation") {
            if let Some(annotation_name) = annotation_display_name(py, &annotation) {
                if annotation_name.contains("Request") {
                    plan.push((name.clone(), InjectionType::Request));
                    needs_request_object = true;
                    special = true;
                } else if annotation_name.contains("SecurityScopes") {
                    plan.push((name.clone(), InjectionType::SecurityScopes));
                    special = true;
                }
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
    cache: &HashMap<u64, SharedPyObject>,
    request_input: &RequestInput,
    request: Option<&SharedPyObject>,
) -> Result<Py<PyDict>, DependencyExecutionError> {
    let final_kwargs = PyDict::new(py);

    for (arg_name, injection_type) in &dep.injection_plan {
        match injection_type {
            InjectionType::Dependency(target_id) => {
                if let Some(cached_val) = cache.get(target_id) {
                    final_kwargs.set_item(arg_name, cached_val.as_ref().bind(py))?;
                }
            }
            InjectionType::Parameter(parameter) => {
                if let Some(value) =
                    pydantic::resolve_parameter_value(py, parameter, request_input)
                        .map_err(DependencyExecutionError::Response)?
                {
                    final_kwargs.set_item(arg_name, value)?;
                }
            }
            InjectionType::Request => {
                if let Some(req) = request {
                    final_kwargs.set_item(arg_name, req.as_ref().bind(py))?;
                }
            }
            InjectionType::SecurityScopes => {
                let py_scopes = Py::new(py, PySecurityScopes::new(Some(dep.scopes.clone())))?;
                final_kwargs.set_item(arg_name, py_scopes)?;
            }
        }
    }

    Ok(final_kwargs.unbind())
}

pub async fn execute_dependencies(
    flat_plan: &[DependencyNode],
    request_input: &RequestInput,
    request: Option<Py<PyAny>>,
) -> Result<HashMap<String, SharedPyObject>, DependencyExecutionError> {
    let request = request.map(Arc::new);
    let mut cache: HashMap<u64, SharedPyObject> = HashMap::with_capacity(flat_plan.len());
    let mut final_results: HashMap<String, SharedPyObject> = HashMap::with_capacity(
        flat_plan
            .iter()
            .filter(|node| node.is_top_level && node.param_name.is_some())
            .count(),
    );

    for dep in flat_plan {
        if dep.use_cache {
            if let Some(cached_val) = cache.get(&dep.func_id) {
                if dep.is_top_level {
                    if let Some(name) = &dep.param_name {
                        final_results.insert(name.clone(), Arc::clone(cached_val));
                    }
                }
                continue;
            }
        }

        let result: SharedPyObject = if dep.is_async {
            let future = Python::attach(|py| -> Result<_, DependencyExecutionError> {
                let py_kwargs =
                    build_dependency_kwargs(py, dep, &cache, request_input, request.as_ref())?;
                let bound_func = dep.func.bind(py);
                let bound_kwargs = py_kwargs.bind(py);
                let coroutine = bound_func.call((), Some(bound_kwargs))?;
                pyo3_async_runtimes::tokio::into_future(coroutine)
                    .map_err(DependencyExecutionError::Python)
            })?;
            Arc::new(future.await.map_err(DependencyExecutionError::Python)?)
        } else {
            Arc::new(Python::attach(
                |py| -> Result<Py<PyAny>, DependencyExecutionError> {
                    let py_kwargs =
                        build_dependency_kwargs(py, dep, &cache, request_input, request.as_ref())?;
                    let bound_func = dep.func.bind(py);
                    let bound_kwargs = py_kwargs.bind(py);
                    Ok(bound_func.call((), Some(bound_kwargs))?.unbind())
                },
            )?)
        };

        if dep.is_top_level {
            if let Some(name) = &dep.param_name {
                final_results.insert(name.clone(), Arc::clone(&result));
            }
        }

        if dep.use_cache {
            cache.insert(dep.func_id, result);
        }
    }

    Ok(final_results)
}
