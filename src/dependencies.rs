use crate::security::PySecurityScopes;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub enum InjectionType {
    /// Holds the `func_id` (pointer address) of the sub-dependency for O(1) cache lookup
    Dependency(u64),
    /// A path/query parameter
    Parameter,
    /// The Request object
    Request,
    /// Security Scopes
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
}

/// Parses dependencies and flattens them into a topologically sorted list
/// (post-order traversal). Sub-dependencies always appear before their parents,
/// so execution is a simple linear `for` loop with no recursion at request time.
pub fn parse_dependencies(py: Python, func: &Bound<PyAny>) -> PyResult<Vec<DependencyNode>> {
    let mut flat_plan = Vec::new();
    let mut visited = HashSet::new();
    extract_and_flatten(py, func, true, None, &mut flat_plan, &mut visited)?;
    Ok(flat_plan)
}

fn extract_and_flatten(
    py: Python,
    func: &Bound<PyAny>,
    is_top_level: bool,
    parent_param_name: Option<String>,
    flat_plan: &mut Vec<DependencyNode>,
    visited: &mut HashSet<u64>,
) -> PyResult<()> {
    let inspect = py.import("inspect")?;
    let signature = inspect.call_method1("signature", (func,))?;
    let parameters = signature.getattr("parameters")?;
    let params_dict = parameters.cast::<PyDict>()?;

    let mut current_scopes: Vec<String> = Vec::new();
    let mut use_cache = true;
    let mut sub_deps: Vec<(String, u64)> = Vec::new();

    for (param_name, param_obj) in params_dict.iter() {
        let param_name_str = param_name.extract::<String>()?;

        if param_name_str == "self" || param_name_str == "cls" || param_name_str == "return" {
            continue;
        }

        if let Ok(default) = param_obj.getattr("default") {
            if !default.is_none() {
                let type_name = default.get_type().name()?.to_string();

                if type_name == "Depends" || type_name == "Security" {
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

                    if type_name == "Security" {
                        if let Ok(s) = default.getattr("scopes") {
                            if let Ok(scope_list) = s.extract::<Vec<String>>() {
                                current_scopes = scope_list;
                            }
                        }
                    }

                    use_cache = default
                        .getattr("use_cache")?
                        .extract::<bool>()
                        .unwrap_or(true);

                    let target_id = target_callable.as_ptr() as u64;
                    sub_deps.push((param_name_str.clone(), target_id));

                    // Recurse first: sub-dependencies are inserted into flat_plan
                    // BEFORE the current function (post-order / topological sort)
                    if !visited.contains(&target_id) {
                        extract_and_flatten(
                            py,
                            &target_callable,
                            false,
                            Some(param_name_str),
                            flat_plan,
                            visited,
                        )?;
                    }
                }
            }
        }
    }

    let func_id = func.as_ptr() as u64;

    if visited.contains(&func_id) {
        return Ok(());
    }

    let injection_plan = build_injection_plan(py, func, &sub_deps)?;
    let is_async = inspect
        .call_method1("iscoroutinefunction", (func,))?
        .extract::<bool>()?;

    // Post-order insert: this node goes in AFTER all its sub-dependencies
    flat_plan.push(DependencyNode {
        func_id,
        func: func.as_unbound().clone(),
        is_async,
        param_name: parent_param_name,
        scopes: current_scopes,
        use_cache,
        is_top_level,
        injection_plan,
    });
    visited.insert(func_id);

    Ok(())
}

fn build_injection_plan(
    py: Python,
    func: &Bound<PyAny>,
    sub_deps: &[(String, u64)],
) -> PyResult<Vec<(String, InjectionType)>> {
    let mut plan = Vec::new();
    let inspect = py.import("inspect")?;
    let signature = inspect.call_method1("signature", (func,))?;
    let parameters_any = signature.getattr("parameters")?;
    let parameters = parameters_any.cast::<PyDict>()?;

    for (key, param) in parameters.iter() {
        let name: String = key.extract()?;

        // If this param is a sub-dependency, record its func_id for O(1) cache lookup
        if let Some((_, target_id)) = sub_deps.iter().find(|(n, _)| n == &name) {
            plan.push((name, InjectionType::Dependency(*target_id)));
            continue;
        }

        let mut special = false;

        if let Ok(ann) = param.getattr("annotation") {
            if let Ok(type_name) = ann.str() {
                let t = type_name.to_string_lossy();
                if t.contains("Request") {
                    plan.push((name.clone(), InjectionType::Request));
                    special = true;
                } else if t.contains("SecurityScopes") {
                    plan.push((name.clone(), InjectionType::SecurityScopes));
                    special = true;
                }
            }
        }

        if !special {
            plan.push((name, InjectionType::Parameter));
        }
    }

    Ok(plan)
}

/// Executes the flat dependency plan linearly.
/// Because the plan is topologically sorted, each dependency's sub-dependencies
/// are guaranteed to be in `cache` before it runs. No recursion, no boxing.
pub async fn execute_dependencies(
    flat_plan: &[DependencyNode],
    request_data: Py<PyDict>,
) -> PyResult<HashMap<String, Py<PyAny>>> {
    let mut cache: HashMap<u64, Py<PyAny>> = HashMap::with_capacity(flat_plan.len());
    let mut final_results: HashMap<String, Py<PyAny>> = HashMap::new();

    for dep in flat_plan {
        // Cache hit: sub-dependency already resolved, just propagate if top-level
        if dep.use_cache {
            if let Some(cached_val) = cache.get(&dep.func_id) {
                if dep.is_top_level {
                    if let Some(name) = &dep.param_name {
                        final_results.insert(name.clone(), cached_val.clone());
                    }
                }
                continue;
            }
        }

        // Build kwargs by borrowing directly from cache — no cloning of cached values
        let (py_kwargs, is_async) = Python::attach(|py| -> PyResult<(Py<PyDict>, bool)> {
            let final_kwargs = PyDict::new(py);
            let req_dict = request_data.bind(py);

            for (arg_name, injection_type) in &dep.injection_plan {
                match injection_type {
                    InjectionType::Dependency(target_id) => {
                        // Borrow from cache directly — zero atomic overhead
                        if let Some(cached_val) = cache.get(target_id) {
                            final_kwargs.set_item(arg_name, cached_val.bind(py))?;
                        }
                    }
                    InjectionType::Parameter => {
                        let mut found = false;
                        if let Ok(Some(params)) = req_dict.get_item("path_params") {
                            if let Ok(dict) = params.cast::<PyDict>() {
                                if let Some(val) = dict.get_item(arg_name)? {
                                    final_kwargs.set_item(arg_name, val)?;
                                    found = true;
                                }
                            }
                        }
                        if !found {
                            if let Ok(Some(params)) = req_dict.get_item("query_params") {
                                if let Ok(dict) = params.cast::<PyDict>() {
                                    if let Some(val) = dict.get_item(arg_name)? {
                                        final_kwargs.set_item(arg_name, val)?;
                                    }
                                }
                            }
                        }
                    }
                    InjectionType::Request => {
                        if let Ok(Some(req)) = req_dict.get_item("request") {
                            final_kwargs.set_item(arg_name, req)?;
                        }
                    }
                    InjectionType::SecurityScopes => {
                        let scopes_obj = PySecurityScopes::new(Some(dep.scopes.clone()));
                        let py_scopes = Py::new(py, scopes_obj)?;
                        final_kwargs.set_item(arg_name, py_scopes)?;
                    }
                }
            }

            Ok((final_kwargs.into(), dep.is_async))
        })?;

        // Call the dependency function (async or sync)
        let result: Py<PyAny> = if is_async {
            let future = Python::attach(|py| -> PyResult<_> {
                let bound_func = dep.func.bind(py);
                let bound_kwargs = py_kwargs.bind(py);
                let coro = bound_func.call((), Some(bound_kwargs))?;
                pyo3_async_runtimes::tokio::into_future(coro)
            })?;
            future.await?
        } else {
            Python::attach(|py| -> PyResult<Py<PyAny>> {
                let bound_func = dep.func.bind(py);
                let bound_kwargs = py_kwargs.bind(py);
                Ok(bound_func.call((), Some(bound_kwargs))?.into())
            })?
        };

        // Propagate top-level results to the route handler
        if dep.is_top_level {
            if let Some(name) = &dep.param_name {
                final_results.insert(name.clone(), result.clone());
            }
        }

        if dep.use_cache {
            cache.insert(dep.func_id, result);
        }
    }

    Ok(final_results)
}
