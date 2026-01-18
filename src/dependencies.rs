use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use std::collections::HashMap;
use tracing::debug;

#[derive(Clone)]
pub struct DependencyInfo {
    pub func: Py<PyAny>,
    pub is_async: bool,
    pub param_name: String,
}

pub fn parse_dependencies(
    py: Python,
    func: &Bound<PyAny>,
) -> PyResult<HashMap<String, DependencyInfo>> {
    let mut deps = HashMap::new();

    let inspect = py.import("inspect")?;
    let signature = inspect.call_method1("signature", (func,))?;
    let parameters = signature.getattr("parameters")?;

    if let Ok(params_dict) = parameters.cast::<PyDict>() {
        for (param_name, param_obj) in params_dict.iter() {
            let param_name_str = param_name.extract::<String>()?;

            if param_name_str == "self" || param_name_str == "return" {
                continue;
            }

            if let Ok(default) = param_obj.getattr("default") {
                if !default.is_none() {
                    let type_obj = default.get_type();
                    let type_name = type_obj.name()?;

                    if type_name == "Depends" {
                        if let Ok(dependency) = default.getattr("dependency") {
                            let is_async = inspect
                                .call_method1("iscoroutinefunction", (dependency.clone(),))?
                                .extract::<bool>()?;

                            debug!(
                                "Found dependency '{}' (async: {})",
                                param_name_str, is_async
                            );

                            deps.insert(
                                param_name_str.clone(),
                                DependencyInfo {
                                    func: dependency.into(),
                                    is_async,
                                    param_name: param_name_str,
                                },
                            );
                        }
                    }
                }
            }
        }
    }

    Ok(deps)
}

pub async fn execute_dependencies(
    dependencies: HashMap<String, DependencyInfo>,
    request_data: Py<PyDict>,
) -> PyResult<HashMap<String, Py<PyAny>>> {
    let mut results = HashMap::new();

    for (name, dep_info) in dependencies {
        debug!("Executing dependency: {}", name);

        let result = if dep_info.is_async {
            let func = dep_info.func.clone();
            let req_data = request_data.clone();
            let future = Python::attach(|py| -> PyResult<_> {
                let args = (req_data,);
                let coroutine = func.call1(py, args)?;
                pyo3_async_runtimes::tokio::into_future(coroutine.into_bound(py))
            })?;
            future.await?.into()
        } else {
            // sync dependency
            Python::attach(|py| -> PyResult<Py<PyAny>> {
                let args = (request_data.clone(),);
                dep_info.func.call1(py, args)
            })?
        };

        results.insert(name, result);
    }

    Ok(results)
}

// TODO: Implement recursive dependency resolution (topological sort)
pub fn resolve_dependency_tree(
    _py: Python,
    dependencies: &HashMap<String, DependencyInfo>,
) -> PyResult<Vec<String>> {
    Ok(dependencies.keys().cloned().collect())
}
