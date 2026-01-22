use crate::security::PySecurityScopes;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyTuple};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub enum InjectionType {
    /// A regular dependency provided by the dependency graph (key is the func_id or name)
    Dependency(String),
    /// A path/query parameter
    Parameter,
    /// The Request object
    Request,
    /// Security Scopes
    SecurityScopes,
    // BackgroundTasks
}

#[derive(Clone, Debug)]
pub struct DependencyInfo {
    pub func: Py<PyAny>,
    pub is_async: bool,
    pub param_name: Option<String>,
    pub scopes: Vec<String>,
    pub use_cache: bool,
    pub sub_dependencies: Vec<DependencyInfo>,
    pub injection_plan: Vec<(String, InjectionType)>,
}

/// Recursively parses a python function to find all Depends/Security arguments
/// AND pre-calculates the injection plan.
pub fn parse_dependencies(py: Python, func: &Bound<PyAny>) -> PyResult<Vec<DependencyInfo>> {
    let mut dependencies = Vec::new();
    let inspect = py.import("inspect")?;
    let signature = inspect.call_method1("signature", (func,))?;
    let parameters = signature.getattr("parameters")?;

    if let Ok(params_dict) = parameters.cast::<PyDict>() {
        for (param_name, param_obj) in params_dict.iter() {
            let param_name_str = param_name.extract::<String>()?;

            if param_name_str == "self" || param_name_str == "cls" || param_name_str == "return" {
                continue;
            }

            if let Ok(default) = param_obj.getattr("default") {
                if !default.is_none() {
                    let type_name = default.get_type().name()?.to_string();

                    if type_name == "Depends" || type_name == "Security" {
                        if let Ok(dependency_callable) = default.getattr("dependency") {
                            let target_callable: Py<PyAny> = if dependency_callable.is_none() {
                                if let Ok(annotation) = param_obj.getattr("annotation") {
                                    annotation.into()
                                } else {
                                    continue;
                                }
                            } else {
                                dependency_callable.into()
                            };

                            let is_async = inspect
                                .call_method1("iscoroutinefunction", (target_callable.clone(),))?
                                .extract::<bool>()?;

                            let mut scopes = Vec::new();
                            if type_name == "Security" {
                                if let Ok(s) = default.getattr("scopes") {
                                    if let Ok(scope_list) = s.extract::<Vec<String>>() {
                                        scopes = scope_list;
                                    }
                                }
                            }

                            let bound_target = target_callable.bind(py);

                            let sub_deps = parse_dependencies(py, bound_target)?;

                            let injection_plan =
                                build_injection_plan(py, bound_target, &sub_deps, &scopes)?;

                            dependencies.push(DependencyInfo {
                                func: target_callable,
                                is_async,
                                param_name: Some(param_name_str),
                                scopes,
                                use_cache: default
                                    .getattr("use_cache")?
                                    .extract::<bool>()
                                    .unwrap_or(true),
                                sub_dependencies: sub_deps,
                                injection_plan,
                            });
                        }
                    }
                }
            }
        }
    }
    Ok(dependencies)
}

fn build_injection_plan(
    py: Python,
    func: &Bound<PyAny>,
    sub_deps: &[DependencyInfo],
    _scopes: &[String],
) -> PyResult<Vec<(String, InjectionType)>> {
    let mut plan = Vec::new();
    let inspect = py.import("inspect")?;
    let signature = inspect.call_method1("signature", (func,))?;
    let parameters_any = signature.getattr("parameters")?;
    let parameters = parameters_any.cast::<PyDict>()?;

    for (key, param) in parameters.iter() {
        let name: String = key.extract()?;

        if sub_deps
            .iter()
            .any(|d| d.param_name.as_deref() == Some(&name))
        {
            plan.push((name.clone(), InjectionType::Dependency(name)));
            continue;
        }

        let annotation = param.getattr("annotation").ok();
        let mut special = false;

        if let Some(ann) = annotation {
            if let Ok(type_name) = ann.str() {
                let t = type_name.to_string();
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

pub async fn execute_dependencies(
    deps: Vec<DependencyInfo>,
    request_data: Py<PyDict>,
) -> PyResult<HashMap<String, Py<PyAny>>> {
    let mut results = HashMap::new();
    let mut cache: HashMap<u64, Py<PyAny>> = HashMap::new();

    for dep in deps {
        let val = resolve_dependency(&dep, &request_data, &mut cache).await?;
        if let Some(name) = dep.param_name {
            results.insert(name, val);
        }
    }
    Ok(results)
}

#[async_recursion::async_recursion]
async fn resolve_dependency(
    dep: &DependencyInfo,
    request_data: &Py<PyDict>,
    cache: &mut HashMap<u64, Py<PyAny>>,
) -> PyResult<Py<PyAny>> {
    let func_id = dep.func.as_ptr() as u64;

    // cache hit
    if dep.use_cache {
        if let Some(cached_val) = cache.get(&func_id) {
            return Ok(cached_val.clone());
        }
    }

    // resolve sub-dependencies
    let mut resolved_subs = HashMap::new();
    for sub in &dep.sub_dependencies {
        let sub_result = resolve_dependency(sub, request_data, cache).await?;
        if let Some(name) = &sub.param_name {
            resolved_subs.insert(name.clone(), sub_result);
        }
    }

    // argument building
    let (py_kwargs, is_async, func): (Py<PyDict>, bool, Py<PyAny>) =
        Python::attach(|py| -> PyResult<(Py<PyDict>, bool, Py<PyAny>)> {
            let final_kwargs = PyDict::new(py);
            let req_dict = request_data.bind(py);

            for (arg_name, injection_type) in &dep.injection_plan {
                match injection_type {
                    InjectionType::Dependency(name) => {
                        if let Some(val) = resolved_subs.get(name) {
                            final_kwargs.set_item(arg_name, val)?;
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

            Ok((final_kwargs.into(), dep.is_async, dep.func.clone()))
        })?;

    // call fn
    let result = if is_async {
        let future = Python::attach(|py| -> PyResult<_> {
            let bound_func = func.bind(py);
            let bound_kwargs = py_kwargs.bind(py);
            let coro = bound_func.call((), Some(bound_kwargs))?;
            pyo3_async_runtimes::tokio::into_future(coro)
        })?;

        future.await?
    } else {
        Python::attach(|py| -> PyResult<Py<PyAny>> {
            let bound_func = func.bind(py);
            let bound_kwargs = py_kwargs.bind(py);
            let res = bound_func.call((), Some(bound_kwargs))?;
            Ok(res.into())
        })?
    };

    // cache
    if dep.use_cache {
        cache.insert(func_id, result.clone());
    }

    Ok(result)
}
