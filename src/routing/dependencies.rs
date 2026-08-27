use super::params;
use super::security::PySecurityScopes;
use super::types::{ParsedParameter, RequestInput};
use crate::ffi::pydantic;

use axum::response::Response;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule, PyString, PyTuple};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::sync::Arc;

type SharedPyObject = Py<PyAny>;

crate::cached_py_import!(INSPECT_MODULE, "inspect");

fn get_inspect(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    INSPECT_MODULE.get(py)
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
    Parameter(Box<ParsedParameter>),
    Request,
    SecurityScopes,
}

#[derive(Clone, Debug)]
pub struct DependencyNode {
    pub func_id: u64,
    pub func: Py<PyAny>,
    pub is_async: bool,
    pub is_generator: bool,
    pub is_async_generator: bool,
    /// Position of this node inside the flattened plan.
    pub index: usize,
    pub param_name: Option<Py<PyString>>,
    pub scopes: Vec<String>,
    pub use_cache: bool,
    pub is_top_level: bool,
    pub injection_plan: Vec<(Py<PyString>, InjectionType)>,
    pub needs_request_object: bool,
}

impl DependencyNode {
    #[inline]
    pub fn is_sync_callable(&self) -> bool {
        !self.is_async && !self.is_async_generator
    }
}

pub struct TeardownTask {
    pub generator: Py<PyAny>,
    pub is_async: bool,
}

pub type TeardownTasks = SmallVec<[TeardownTask; 4]>;
pub type ResolvedDependency = (Py<PyString>, SharedPyObject);
pub type ResolvedDependencies = SmallVec<[ResolvedDependency; 4]>;
pub type ResultRegistry = SmallVec<[Option<SharedPyObject>; 8]>;

pub enum DependencyExecutionError {
    Python(PyErr),
    Response(Box<Response>),
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
        && is_coroutine.is_truthy().unwrap_or(false)
    {
        return true;
    }
    if let Ok(call_method) = func.getattr(intern!(py, "__call__"))
        && let Ok(is_coroutine) =
            inspect.call_method1(intern!(py, "iscoroutinefunction"), (call_method,))
        && is_coroutine.is_truthy().unwrap_or(false)
    {
        return true;
    }
    false
}

fn is_generator_callable(
    py: Python<'_>,
    inspect: &Bound<'_, PyModule>,
    func: &Bound<'_, PyAny>,
) -> bool {
    if let Ok(is_gen) = inspect.call_method1(intern!(py, "isgeneratorfunction"), (func,))
        && is_gen.is_truthy().unwrap_or(false)
    {
        return true;
    }
    if let Ok(call_method) = func.getattr(intern!(py, "__call__"))
        && let Ok(is_gen) = inspect.call_method1(intern!(py, "isgeneratorfunction"), (call_method,))
        && is_gen.is_truthy().unwrap_or(false)
    {
        return true;
    }
    false
}

fn is_async_generator_callable(
    py: Python<'_>,
    inspect: &Bound<'_, PyModule>,
    func: &Bound<'_, PyAny>,
) -> bool {
    if let Ok(is_gen) = inspect.call_method1(intern!(py, "isasyncgenfunction"), (func,))
        && is_gen.is_truthy().unwrap_or(false)
    {
        return true;
    }
    if let Ok(call_method) = func.getattr(intern!(py, "__call__"))
        && let Ok(is_gen) = inspect.call_method1(intern!(py, "isasyncgenfunction"), (call_method,))
        && is_gen.is_truthy().unwrap_or(false)
    {
        return true;
    }
    false
}

fn annotation_display_name(py: Python<'_>, annotation: &Bound<'_, PyAny>) -> Option<String> {
    annotation
        .getattr(intern!(py, "__name__"))
        .ok()
        .and_then(|value| value.extract::<String>().ok())
        .or_else(|| {
            annotation
                .str()
                .ok()
                .map(|value| value.to_string_lossy().into_owned())
        })
}

fn extract_string_list(value: &Bound<'_, PyAny>) -> Option<Vec<String>> {
    value.try_iter().ok().map(|iter| {
        iter.filter_map(|item| item.ok()?.extract::<String>().ok())
            .collect()
    })
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
    adapt_security_schemes(py, &mut flat_plan);
    Ok(flat_plan)
}
fn adapt_security_schemes(py: Python<'_>, nodes: &mut [DependencyNode]) {
    for node in nodes.iter_mut() {
        if crate::routing::security::describe_scheme(py, node.func.bind(py)).is_some() {
            node.injection_plan = vec![(
                PyString::intern(py, "request").unbind(),
                InjectionType::Request,
            )];
            node.needs_request_object = true;
        }
    }
}

pub fn parse_external_dependencies(
    py: Python<'_>,
    callables: &[Py<PyAny>],
    path_param_names: &[String],
) -> PyResult<Vec<DependencyNode>> {
    let inspect = get_inspect(py)?;
    let keys = ParserKeys::new(py);
    let mut flat_plan = Vec::new();
    let mut visited = HashMap::new();

    for callable in callables {
        let bound = callable.bind(py);
        extract_and_flatten(
            py,
            &inspect,
            &keys,
            bound,
            path_param_names,
            true,
            None,
            Vec::new(),
            true,
            &mut flat_plan,
            &mut visited,
        )?;
    }

    adapt_security_schemes(py, &mut flat_plan);
    Ok(flat_plan)
}

pub fn shift_dependency_indices(nodes: &mut [DependencyNode], offset: usize) {
    for node in nodes.iter_mut() {
        node.index += offset;
        for (_, injection) in node.injection_plan.iter_mut() {
            if let InjectionType::Dependency(target) = injection {
                *target += offset;
            }
        }
    }
}

pub(crate) fn override_node_callable(
    py: Python<'_>,
    node: &mut DependencyNode,
    replacement: &Py<PyAny>,
) {
    node.func = replacement.clone_ref(py);
    if let Ok(inspect) = INSPECT_MODULE.get(py) {
        let bound = node.func.bind(py);
        node.is_async = is_async_callable(py, &inspect, bound);
        node.is_generator = is_generator_callable(py, &inspect, bound);
        node.is_async_generator = is_async_generator_callable(py, &inspect, bound);
    }
}

pub fn collect_dependency_callables(
    py: Python<'_>,
    dependencies: Option<&Py<PyAny>>,
) -> Vec<Py<PyAny>> {
    dependencies
        .and_then(|deps| deps.bind(py).try_iter().ok())
        .map(|iter| iter.flatten().map(Bound::unbind).collect())
        .unwrap_or_default()
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

    if use_cache && let Some(&idx) = visited.get(&func_id) {
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
        let param_name_str: &str = param_name.cast::<PyString>()?.to_str()?;

        if param_name_str == "self" || param_name_str == "cls" || param_name_str == "return" {
            continue;
        }

        if let Ok(default) = param_obj.getattr(keys.default) {
            let default_is_empty = params::is_inspect_empty(py, &default);
            let marker: Option<Bound<'_, PyAny>> = if default_is_empty {
                params::find_annotated_dependency_marker(&param_obj)
            } else {
                Some(default)
            };

            let Some(marker) = marker else {
                continue;
            };

            let is_depends = marker.hasattr(keys.dependency).unwrap_or(false);
            let is_security = marker.hasattr(keys.scopes).unwrap_or(false);

            if !is_depends && !is_security {
                continue;
            }

            let target_callable = if let Ok(dep) = marker.getattr(keys.dependency) {
                if dep.is_none() {
                    if let Ok(annotation) = param_obj.getattr(keys.annotation) {
                        params::base_annotation(py, &annotation)
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
                marker
                    .getattr(keys.scopes)
                    .ok()
                    .and_then(|value| extract_string_list(&value))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            let child_use_cache = marker
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
                Some(param_name_str.to_owned()),
                child_scopes,
                child_use_cache,
                flat_plan,
                visited,
            )?;

            sub_deps.push((param_name_str.to_owned(), target_index));
        }
    }

    let (injection_plan, needs_request_object) =
        build_injection_plan(py, func, path_param_names, &sub_deps, inspect, keys)?;
    let injection_plan = injection_plan
        .into_iter()
        .map(|(name, injection)| (PyString::intern(py, &name).unbind(), injection))
        .collect();
    let is_async = is_async_callable(py, inspect, func);
    let is_generator = is_generator_callable(py, inspect, func);
    let is_async_generator = is_async_generator_callable(py, inspect, func);

    let node_index = flat_plan.len();
    flat_plan.push(DependencyNode {
        func_id,
        func: func.as_unbound().clone(),
        is_async,
        is_generator,
        is_async_generator,
        index: node_index,
        param_name: parent_param_name
            .as_deref()
            .map(|name| PyString::intern(py, name).unbind()),
        scopes,
        use_cache,
        is_top_level,
        injection_plan,
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
        let name: String = key.extract()?;
        let param = pair.get_item(1)?;

        if name == "self" || name == "cls" || name == "return" {
            continue;
        }

        if let Some((_, target_idx)) = sub_deps.iter().find(|(param_name, _)| param_name == &name) {
            plan.push((name, InjectionType::Dependency(*target_idx)));
            continue;
        }

        let special_injection = param
            .getattr(keys.annotation)
            .ok()
            .and_then(|ann| annotation_display_name(py, &ann))
            .and_then(|ann_name| {
                if ann_name.contains("Request")
                    || ann_name.contains("HTTPConnection")
                    || ann_name.contains("WebSocket")
                {
                    Some(InjectionType::Request)
                } else if ann_name.contains("SecurityScopes") {
                    Some(InjectionType::SecurityScopes)
                } else {
                    None
                }
            });

        match special_injection {
            Some(InjectionType::Request) => {
                plan.push((name, InjectionType::Request));
                needs_request_object = true;
            }
            Some(injection) => {
                plan.push((name, injection));
            }
            None => {
                let parsed_param =
                    params::parse_parameter_spec(py, &name, &param, path_param_names)?;
                plan.push((name, InjectionType::Parameter(Box::new(parsed_param))));
            }
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
                    .map_err(|e| DependencyExecutionError::Response(Box::new(e)))?
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
) -> Result<(ResolvedDependencies, TeardownTasks), DependencyExecutionError> {
    let request = request;
    let mut results_registry: ResultRegistry = smallvec::smallvec![None; flat_plan.len()];
    let mut teardown_tasks = TeardownTasks::new();

    let mut final_results = ResolvedDependencies::with_capacity(
        flat_plan
            .iter()
            .filter(|node| node.is_top_level && node.param_name.is_some())
            .count(),
    );

    for dep in flat_plan {
        let (result, teardown) =
            execute_sync_node(py, dep, &results_registry, request_input, request.as_ref())?;
        if let Some(teardown) = teardown {
            teardown_tasks.push(teardown);
        }

        results_registry[dep.index] = Some(result.clone_ref(py));

        if dep.is_top_level
            && let Some(name) = &dep.param_name
        {
            final_results.push((name.clone_ref(py), result));
        }
    }

    Ok((final_results, teardown_tasks))
}

pub async fn execute_dependencies(
    async_loop: &Arc<Py<PyAny>>,
    flat_plan: &[DependencyNode],
    request_input: &RequestInput<'_>,
    request: Option<Py<PyAny>>,
) -> Result<(ResolvedDependencies, TeardownTasks), DependencyExecutionError> {
    let mut results_registry: ResultRegistry = smallvec::smallvec![None; flat_plan.len()];
    let mut teardown_tasks = TeardownTasks::new();

    let mut final_results = ResolvedDependencies::with_capacity(
        flat_plan
            .iter()
            .filter(|node| node.is_top_level && node.param_name.is_some())
            .count(),
    );

    let mut idx = 0;
    while idx < flat_plan.len() {
        let dep = &flat_plan[idx];

        if dep.is_sync_callable() {
            let run_end = idx
                + flat_plan[idx..]
                    .iter()
                    .take_while(|node| node.is_sync_callable())
                    .count();

            Python::attach(|py| -> Result<(), DependencyExecutionError> {
                for node in &flat_plan[idx..run_end] {
                    let (result, teardown) = execute_sync_node(
                        py,
                        node,
                        &results_registry,
                        request_input,
                        request.as_ref(),
                    )?;
                    if let Some(teardown) = teardown {
                        teardown_tasks.push(teardown);
                    }
                    register_result(py, node, result, &mut results_registry, &mut final_results);
                }
                Ok(())
            })?;

            idx = run_end;
            continue;
        }

        if dep.is_async_generator {
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
                let generator = bound_func.call((), Some(bound_kwargs))?;

                let anext_coroutine = generator.call_method0(intern!(py, "__anext__"))?;

                let locals = rsloop::rust_async::TaskLocals::new(async_loop.bind(py).clone());
                Ok((
                    generator.unbind(),
                    rsloop::rust_async::into_future_with_locals(&locals, anext_coroutine)?,
                ))
            })?;

            let (generator, future_anext) = future;
            let outcome = future_anext.await;
            Python::attach(|py| -> Result<(), DependencyExecutionError> {
                let value = match outcome {
                    Ok(v) => v,
                    Err(e) => {
                        return Err(
                            if e.is_instance_of::<pyo3::exceptions::PyStopAsyncIteration>(py) {
                                DependencyExecutionError::Python(
                                    pyo3::exceptions::PyRuntimeError::new_err(
                                        "Async generator dependency exited before yielding",
                                    ),
                                )
                            } else {
                                DependencyExecutionError::Python(e)
                            },
                        );
                    }
                };
                teardown_tasks.push(TeardownTask {
                    generator: generator.clone_ref(py),
                    is_async: true,
                });
                register_result(py, dep, value, &mut results_registry, &mut final_results);
                Ok(())
            })?;
        } else {
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
                let coroutine = bound_func.call((), Some(bound_kwargs))?;
                let locals = rsloop::rust_async::TaskLocals::new(async_loop.bind(py).clone());
                Ok(rsloop::rust_async::into_future_with_locals(
                    &locals, coroutine,
                )?)
            })?;
            let outcome = future.await;

            Python::attach(|py| -> Result<(), DependencyExecutionError> {
                let result = outcome.map_err(DependencyExecutionError::Python)?;
                register_result(py, dep, result, &mut results_registry, &mut final_results);
                Ok(())
            })?;
        }

        idx += 1;
    }

    Ok((final_results, teardown_tasks))
}

#[inline]
fn register_result(
    py: Python<'_>,
    dep: &DependencyNode,
    result: SharedPyObject,
    results_registry: &mut [Option<SharedPyObject>],
    final_results: &mut ResolvedDependencies,
) {
    results_registry[dep.index] = Some(result.clone());
    if dep.is_top_level
        && let Some(name) = &dep.param_name
    {
        final_results.push((name.clone_ref(py), result));
    }
}

#[inline]
fn execute_sync_node(
    py: Python<'_>,
    dep: &DependencyNode,
    results_registry: &[Option<SharedPyObject>],
    request_input: &RequestInput<'_>,
    request: Option<&SharedPyObject>,
) -> Result<(SharedPyObject, Option<TeardownTask>), DependencyExecutionError> {
    let py_kwargs = build_dependency_kwargs(py, dep, results_registry, request_input, request)?;
    let bound_func = dep.func.bind(py);
    let bound_kwargs = py_kwargs.bind(py);

    if dep.is_generator {
        let generator = bound_func.call((), Some(bound_kwargs))?;
        let value = match generator.call_method0(intern!(py, "__next__")) {
            Ok(v) => v.unbind(),
            Err(e) => {
                return Err(
                    if e.is_instance_of::<pyo3::exceptions::PyStopIteration>(py) {
                        DependencyExecutionError::Python(pyo3::exceptions::PyRuntimeError::new_err(
                            "Generator dependency exited before yielding",
                        ))
                    } else {
                        DependencyExecutionError::Python(e)
                    },
                );
            }
        };
        let teardown = TeardownTask {
            generator: generator.unbind(),
            is_async: false,
        };
        Ok((value, Some(teardown)))
    } else {
        Ok((bound_func.call((), Some(bound_kwargs))?.unbind(), None))
    }
}
