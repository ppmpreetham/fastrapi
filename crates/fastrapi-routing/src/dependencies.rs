//! The dependency graph and the dependency execution for routes.
//! This module flattens the `Depends` and `Security` markers of an endpoint into a plan of dependency nodes.
//! For each request, it runs the plan, keeps the results, and runs the teardown of generator dependencies.

use super::params;
use super::security::PySecurityScopes;
use crate::request_data::{Errors, request_param_value};
use crate::route::ParsedParameter;
use fastrapi_http::request_input::RequestInput;
use fastrapi_http::responses::PyResponse;
use fastrapi_pyutil::call::{CallArgs, CallLayout};
use fastrapi_runtime::py_bridge::EventLoop;
use fastrapi_runtime::{blocking, py_bridge, server_state};
use tokio::runtime::{Handle, RuntimeFlavor};
use tokio::task::block_in_place;

use crate::security::{OAuth2PasswordRequestForm, describe_scheme};
use fastrapi_core::params::ParameterSource;
use fastrapi_core::security::SecurityKind;
use pyo3::exceptions::{
    PyAssertionError, PyBaseException, PyRuntimeError, PyStopAsyncIteration, PyStopIteration,
};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{IntoPyDict, PyAny, PyCFunction, PyDict, PyFunction, PyString, PyTuple, PyType};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::mem::take;
use std::sync::{Arc, OnceLock};

type SharedPyObject = Py<PyAny>;

fastrapi_pyutil::cached_py_import!(ASYNC_EXIT_STACK, "contextlib", "AsyncExitStack");
fastrapi_pyutil::cached_py_import!(CONTEXT_MANAGER, "contextlib", "contextmanager");
fastrapi_pyutil::cached_py_import!(ASYNC_CONTEXT_MANAGER, "contextlib", "asynccontextmanager");
fastrapi_pyutil::cached_py_import!(TO_THREAD, "asyncio", "to_thread");
fastrapi_pyutil::cached_py_import!(
    RUN_COROUTINE_THREADSAFE,
    "asyncio",
    "run_coroutine_threadsafe"
);
fastrapi_pyutil::cached_py_import!(PARTIAL, "functools", "partial");
fastrapi_pyutil::cached_py_import!(STARLETTE_RESPONSE, "starlette.responses", "Response");

struct ParserKeys<'py> {
    parameters: &'py Bound<'py, PyString>,
    items: &'py Bound<'py, PyString>,
    default: &'py Bound<'py, PyString>,
    dependency: &'py Bound<'py, PyString>,
    annotation: &'py Bound<'py, PyString>,
    scopes: &'py Bound<'py, PyString>,
    use_cache: &'py Bound<'py, PyString>,
    scope: &'py Bound<'py, PyString>,
}

/// The `scope` of a `Depends` marker: when the teardown of a generator dependency runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DependencyScope {
    Function,
    Request,
}

impl DependencyScope {
    fn of(value: &Bound<'_, PyAny>) -> Option<Self> {
        match value.extract::<&str>().ok()? {
            "function" => Some(Self::Function),
            "request" => Some(Self::Request),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
struct MarkerOptions {
    use_cache: bool,
    scope: Option<DependencyScope>,
}

impl MarkerOptions {
    const ROOT: Self = Self {
        use_cache: true,
        scope: None,
    };

    fn of(marker: &Bound<'_, PyAny>, keys: &ParserKeys<'_>) -> Self {
        Self {
            use_cache: marker
                .getattr(keys.use_cache)
                .ok()
                .and_then(|value| value.is_truthy().ok())
                .unwrap_or(true),
            scope: marker
                .getattr(keys.scope)
                .ok()
                .and_then(|value| DependencyScope::of(&value)),
        }
    }
}

/// What FastAPI caches a dependency value by: the callable, its sorted scopes when it uses scopes, and its computed scope.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CacheKey {
    call: u64,
    scopes: Vec<String>,
    scope: Option<DependencyScope>,
}

impl CacheKey {
    fn new(call: u64, scopes: Option<&[String]>, scope: Option<DependencyScope>) -> Self {
        let mut scopes = scopes.map(<[String]>::to_vec).unwrap_or_default();
        scopes.sort_unstable();
        scopes.dedup();
        Self {
            call,
            scopes,
            scope,
        }
    }
}

/// The dependencies already flattened, so that a cached dependency is parsed once.
#[derive(Default)]
struct Visited {
    nodes: HashMap<CacheKey, usize>,
    /// Whether a callable uses scopes through its signature, its sub-dependencies or by being a security scheme, as FastAPI's `_uses_scopes`.
    uses_scopes: HashMap<u64, bool>,
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
            scope: intern!(py, "scope"),
        }
    }
}

#[derive(Clone, Debug)]
pub enum InjectionType {
    Dependency(usize),
    Parameter(Box<ParsedParameter>),
    Request,
    Response,
    SecurityScopes,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Dependency graph nodes track multiple execution properties as booleans"
)]
#[derive(Clone, Debug)]
pub struct DependencyNode {
    pub cache_key: CacheKey,
    /// The dependency as the user declared it, used for caching, overrides and `OpenAPI`.
    pub func: Py<PyAny>,
    /// The callable the executor invokes, which is a security scheme's sync `_resolve`.
    pub call: Py<PyAny>,
    pub is_async: bool,
    pub is_generator: bool,
    pub is_async_generator: bool,
    /// The position of this node in the flattened plan.
    pub index: usize,
    pub bindings: SmallVec<[Py<PyString>; 1]>,
    pub scopes: Arc<[String]>,
    pub use_cache: bool,
    pub function_scope: bool,
    pub is_top_level: bool,
    pub injection_plan: Vec<(Py<PyString>, InjectionType)>,
    pub needs_request_object: bool,
    pub context_factory: Arc<OnceLock<Py<PyAny>>>,
    pub call_layout: Option<Arc<CallLayout>>,
}

impl DependencyNode {
    fn context_factory<'py>(&self, py: Python<'py>) -> PyResult<&Bound<'py, PyAny>> {
        if self.context_factory.get().is_none() {
            let decorator = if self.is_async_generator {
                &ASYNC_CONTEXT_MANAGER
            } else {
                &CONTEXT_MANAGER
            };
            let factory = decorator.get(py)?.call1((self.func.bind(py),))?;
            _ = self.context_factory.set(factory.unbind());
        }
        self.context_factory
            .get()
            .map(|factory| factory.bind(py))
            .ok_or_else(|| PyRuntimeError::new_err("dependency context factory is missing"))
    }

    #[inline]
    #[must_use]
    pub const fn is_sync_callable(&self) -> bool {
        !self.is_async && !self.is_async_generator
    }
}

#[derive(Debug)]
pub struct TeardownTask {
    pub generator: Py<PyAny>,
    pub is_async: bool,
    pub function_scope: bool,
}

#[must_use]
pub fn split_function_scope(teardowns: TeardownTasks) -> (TeardownTasks, TeardownTasks) {
    teardowns.into_iter().partition(|task| task.function_scope)
}

pub type TeardownTasks = SmallVec<[TeardownTask; 4]>;
pub type ResolvedDependency = (Py<PyString>, SharedPyObject);
pub type ResolvedDependencies = SmallVec<[ResolvedDependency; 4]>;
pub type ResultRegistry = SmallVec<[Option<SharedPyObject>; 8]>;

fn visit(
    py: Python<'_>,
    index: usize,
    source: &[DependencyNode],
    overrides: &Bound<'_, PyDict>,
    paths: &[String],
    output: &mut Vec<DependencyNode>,
    visited: &mut HashMap<usize, usize>,
) -> PyResult<usize> {
    if let Some(index) = visited.get(&index) {
        return Ok(*index);
    }
    let original = &source[index];
    let mut node = original.clone();
    if let Some(replacement) = overrides.get_item(original.func.bind(py))? {
        let mut replacement_plan = parse_external_dependencies(py, &[replacement.unbind()], paths)?;
        let mut local = HashMap::new();
        let mut replacement_root = replacement_plan
            .pop()
            .ok_or_else(|| PyRuntimeError::new_err("Empty dependency replacement"))?;
        for child in &mut replacement_plan {
            child.is_top_level = false;
        }
        for (_, injection) in &mut replacement_root.injection_plan {
            if let InjectionType::Dependency(child) = injection {
                *child = visit(
                    py,
                    *child,
                    &replacement_plan,
                    overrides,
                    paths,
                    output,
                    &mut local,
                )?;
            }
        }
        let root = output.len();
        replacement_root.index = root;
        replacement_root.cache_key = original.cache_key.clone();
        replacement_root.bindings.clone_from(&original.bindings);
        replacement_root.scopes.clone_from(&original.scopes);
        replacement_root.is_top_level = original.is_top_level;
        replacement_root.use_cache = original.use_cache;
        output.push(replacement_root);
        visited.insert(index, root);
        return Ok(root);
    }
    for (_, injection) in &mut node.injection_plan {
        if let InjectionType::Dependency(child) = injection {
            *child = visit(py, *child, source, overrides, paths, output, visited)?;
        }
    }
    node.index = output.len();
    visited.insert(index, node.index);
    output.push(node);
    Ok(output.len() - 1)
}

pub struct OverridePlan {
    pub nodes: Vec<DependencyNode>,
    pub needs_request: bool,
    pub needs_response: bool,
}

type OverrideSnapshot = Vec<(Py<PyAny>, Py<PyAny>)>;

#[derive(Default)]
pub struct OverrideCache(parking_lot::Mutex<Option<(OverrideSnapshot, Arc<OverridePlan>)>>);

impl Clone for OverrideCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

pub enum RequestPlan<'a> {
    Static(&'a [DependencyNode]),
    Overridden(Arc<OverridePlan>),
}

impl RequestPlan<'_> {
    #[must_use]
    pub fn nodes(&self) -> &[DependencyNode] {
        match self {
            Self::Static(nodes) => nodes,
            Self::Overridden(plan) => &plan.nodes,
        }
    }
}

fn snapshot_matches(snapshot: &OverrideSnapshot, overrides: &Bound<'_, PyDict>) -> bool {
    snapshot.len() == overrides.len()
        && snapshot.iter().zip(overrides.iter()).all(
            |((key, value), (current_key, current_value))| {
                key.is(&current_key) && value.is(&current_value)
            },
        )
}

pub fn request_plan<'a>(
    py: Python<'_>,
    plan: &'a [DependencyNode],
    overrides: Option<&Py<PyAny>>,
    path_names: &[Arc<str>],
    cache: &OverrideCache,
) -> PyResult<RequestPlan<'a>> {
    let Some(overrides) = overrides else {
        return Ok(RequestPlan::Static(plan));
    };
    let overrides = overrides.bind(py).cast::<PyDict>()?;
    if overrides.is_empty() {
        return Ok(RequestPlan::Static(plan));
    }
    if let Some((snapshot, cached)) = cache.0.lock().as_ref()
        && snapshot_matches(snapshot, overrides)
    {
        return Ok(RequestPlan::Overridden(Arc::clone(cached)));
    }
    let nodes = override_nodes(py, plan, overrides, path_names)?;
    let computed = Arc::new(OverridePlan {
        needs_request: nodes.iter().any(|node| node.needs_request_object),
        needs_response: nodes.iter().any(|node| {
            node.injection_plan
                .iter()
                .any(|(_, injection)| matches!(injection, InjectionType::Response))
        }),
        nodes,
    });
    let snapshot = overrides
        .iter()
        .map(|(key, value)| (key.unbind(), value.unbind()))
        .collect();
    *cache.0.lock() = Some((snapshot, Arc::clone(&computed)));
    Ok(RequestPlan::Overridden(computed))
}

fn override_nodes(
    py: Python<'_>,
    plan: &[DependencyNode],
    overrides: &Bound<'_, PyDict>,
    path_names: &[Arc<str>],
) -> PyResult<Vec<DependencyNode>> {
    let paths = path_names
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let mut output = Vec::new();
    let mut visited = HashMap::new();
    for node in plan.iter().filter(|node| node.is_top_level) {
        visit(
            py,
            node.index,
            plan,
            overrides,
            &paths,
            &mut output,
            &mut visited,
        )?;
    }
    Ok(output)
}

enum NodeStep {
    Done,
    Pending {
        future: py_bridge::PyTaskFuture,
        exit: Option<Py<PyAny>>,
    },
}

#[derive(Default)]
pub struct ExitStacks {
    pub request: Option<Py<PyAny>>,
    pub function: Option<Py<PyAny>>,
}

impl ExitStacks {
    const fn slot(&mut self, node: &DependencyNode) -> &mut Option<Py<PyAny>> {
        if node.function_scope {
            &mut self.function
        } else {
            &mut self.request
        }
    }
}

fn exit_stack<'a, 'py>(
    py: Python<'py>,
    slot: &'a mut Option<Py<PyAny>>,
) -> PyResult<&'a Bound<'py, PyAny>> {
    let stack = match slot.take() {
        Some(stack) => stack,
        None => ASYNC_EXIT_STACK.get(py)?.call0()?.unbind(),
    };
    Ok(slot.insert(stack).bind(py))
}

pub async fn execute_request_dependencies(
    event_loop: EventLoop,
    plan: &[DependencyNode],
    input: &RequestInput<'_>,
    request: Option<&Py<PyAny>>,
    response: Option<&Py<PyAny>>,
    stacks: &mut ExitStacks,
    sync_to_threadpool: bool,
) -> PyResult<ResolvedDependencies> {
    let mut registry: ResultRegistry = smallvec::smallvec![None; plan.len()];
    let mut results = ResolvedDependencies::new();
    for node in plan {
        let step = Python::attach(|py| -> PyResult<_> {
            let mut args = dependency_args(py, node, &registry, input, request)?;
            if let Some(response) = response {
                for (name, injection) in &node.injection_plan {
                    if matches!(injection, InjectionType::Response) {
                        args.set(name.bind(py), response.bind(py))?;
                    }
                }
            }
            let func = node.call.bind(py);
            let pending = |future, exit| Ok(NodeStep::Pending { future, exit });
            if node.is_generator || node.is_async_generator {
                let manager = node
                    .context_factory(py)?
                    .call((), Some(&args.to_dict(py)?))?;
                if node.is_generator && sync_to_threadpool {
                    let to_thread = TO_THREAD.get(py)?;
                    let coroutine =
                        to_thread.call1((manager.getattr(intern!(py, "__enter__"))?,))?;
                    let exit = PARTIAL
                        .get(py)?
                        .call1((to_thread, manager.getattr(intern!(py, "__exit__"))?))?;
                    return pending(
                        py_bridge::schedule_task(py, event_loop, coroutine)?,
                        Some(exit.unbind()),
                    );
                }
                let stack = exit_stack(py, stacks.slot(node))?;
                if node.is_async_generator {
                    let entered =
                        stack.call_method1(intern!(py, "enter_async_context"), (manager,))?;
                    return pending(py_bridge::schedule_task(py, event_loop, entered)?, None);
                }
                let value = {
                    let _entered = py_bridge::enter_request_copy(py)?;
                    stack.call_method1(intern!(py, "enter_context"), (manager,))?
                };
                register_result(py, node, value.unbind(), &mut registry, &mut results);
                return Ok(NodeStep::Done);
            }
            if !node.is_async && sync_to_threadpool {
                let call = PARTIAL.get(py)?.call((func,), Some(&args.to_dict(py)?))?;
                let coroutine = TO_THREAD.get(py)?.call1((call,))?;
                return pending(py_bridge::schedule_task(py, event_loop, coroutine)?, None);
            }
            if node.is_async {
                let coroutine = args.call(func, node.call_layout.as_deref())?;
                return pending(py_bridge::schedule_task(py, event_loop, coroutine)?, None);
            }
            let value = {
                let _entered = py_bridge::enter_request_copy(py)?;
                args.call(func, node.call_layout.as_deref())?
            };
            register_result(py, node, value.unbind(), &mut registry, &mut results);
            Ok(NodeStep::Done)
        })?;
        if let NodeStep::Pending { future, exit } = step {
            let value = future.await?;
            Python::attach(|py| -> PyResult<()> {
                if let Some(exit) = exit {
                    exit_stack(py, stacks.slot(node))?
                        .call_method1(intern!(py, "push_async_exit"), (exit,))?;
                }
                register_result(py, node, value, &mut registry, &mut results);
                Ok(())
            })?;
        }
    }
    Ok(results)
}

fn is_connection_annotation(py: Python<'_>, annotation: &Bound<'_, PyAny>) -> bool {
    crate::annotations::connection_kind(py, annotation).is_some()
}

pub(crate) fn is_response_annotation(py: Python<'_>, annotation: &Bound<'_, PyAny>) -> bool {
    let annotation = params::base_annotation(annotation);
    let Ok(class) = annotation.cast::<PyType>() else {
        return false;
    };
    class.is_subclass_of::<PyResponse>().unwrap_or(false)
        || STARLETTE_RESPONSE
            .get(py)
            .is_ok_and(|base| class.is_subclass(base).unwrap_or(false))
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
    let keys = ParserKeys::new(py);
    let mut flat_plan = Vec::new();
    let mut visited = Visited::default();

    extract_and_flatten(
        py,
        &keys,
        func,
        path_param_names,
        Depth::Endpoint,
        None,
        &[],
        &[],
        MarkerOptions::ROOT,
        &mut flat_plan,
        &mut visited,
    )?;

    flat_plan.pop();
    adapt_security_schemes(py, &mut flat_plan);
    Ok(flat_plan)
}
fn adapt_security_schemes(py: Python<'_>, nodes: &mut [DependencyNode]) {
    for node in nodes.iter_mut() {
        let is_form_type = node.func.bind(py).cast::<PyType>().is_ok_and(|ty| {
            ty.is_subclass_of::<OAuth2PasswordRequestForm>()
                .unwrap_or(false)
        });
        let is_form = node
            .func
            .bind(py)
            .is_instance_of::<OAuth2PasswordRequestForm>();
        // The instance only stands in for the class: its `__call__` reads the request's form.
        if is_form_type
            && let Ok(instance) = [
                ("grant_type", "password"),
                ("username", ""),
                ("password", ""),
            ]
            .into_py_dict(py)
            .and_then(|blank| node.func.bind(py).call((), Some(&blank)))
        {
            node.func = instance.unbind();
            node.call = node.func.clone_ref(py);
        }
        let native_scheme = describe_scheme(py, node.func.bind(py))
            .is_some_and(|spec| !matches!(spec.kind, SecurityKind::Model { .. }));
        if native_scheme || is_form || is_form_type {
            node.injection_plan = vec![(
                PyString::intern(py, "request").unbind(),
                InjectionType::Request,
            )];
            node.needs_request_object = true;
        }
        if native_scheme && let Some(resolve) = sync_resolve(node.func.bind(py)) {
            node.call = resolve.unbind();
        }
        node.call_layout = CallLayout::of(node.call.bind(py)).map(Arc::new);
    }
}

/// Returns the sync `_resolve` of a security scheme whose awaitable `__call__` is not overridden in Python.
fn sync_resolve<'py>(scheme: &Bound<'py, PyAny>) -> Option<Bound<'py, PyAny>> {
    let py = scheme.py();
    let call = scheme.get_type().getattr(intern!(py, "__call__")).ok()?;
    if call.is_instance_of::<PyFunction>() {
        return None;
    }
    scheme.getattr(intern!(py, "_resolve")).ok()
}

pub fn parse_external_dependencies(
    py: Python<'_>,
    markers: &[Py<PyAny>],
    path_param_names: &[String],
) -> PyResult<Vec<DependencyNode>> {
    let keys = ParserKeys::new(py);
    let mut flat_plan = Vec::new();
    let mut visited = Visited::default();

    for marker in markers {
        let marker = marker.bind(py);
        let callable = marker
            .getattr(keys.dependency)
            .ok()
            .filter(|dependency| !dependency.is_none())
            .unwrap_or_else(|| marker.clone());
        if params::is_dependency_marker(marker) && !callable.is_callable() {
            return Err(PyAssertionError::new_err(
                "A parameter-less dependency must have a callable dependency",
            ));
        }
        let scopes = marker
            .getattr(keys.scopes)
            .ok()
            .and_then(|value| extract_string_list(&value))
            .unwrap_or_default();
        extract_and_flatten(
            py,
            &keys,
            &callable,
            path_param_names,
            Depth::TopLevel,
            None,
            &[],
            &scopes,
            MarkerOptions::of(marker, &keys),
            &mut flat_plan,
            &mut visited,
        )?;
    }

    adapt_security_schemes(py, &mut flat_plan);
    Ok(flat_plan)
}

#[must_use]
pub fn merge_dependencies(
    mut merged: Vec<DependencyNode>,
    nodes: Vec<DependencyNode>,
) -> Vec<DependencyNode> {
    let mut positions: SmallVec<[usize; 8]> = SmallVec::with_capacity(nodes.len());
    for mut node in nodes {
        for (_, injection) in &mut node.injection_plan {
            if let InjectionType::Dependency(target) = injection {
                *target = positions[*target];
            }
        }
        let cached = node
            .use_cache
            .then(|| {
                merged
                    .iter()
                    .position(|other| other.cache_key == node.cache_key)
            })
            .flatten();
        let position = if let Some(position) = cached {
            let target = &mut merged[position];
            target.is_top_level |= node.is_top_level;
            target.bindings.extend(node.bindings);
            position
        } else {
            node.index = merged.len();
            merged.push(node);
            merged.len() - 1
        };
        positions.push(position);
    }
    merged
}

#[must_use]
pub fn collect_dependency_markers(
    py: Python<'_>,
    dependencies: Option<&Py<PyAny>>,
) -> Vec<Py<PyAny>> {
    dependencies
        .and_then(|deps| deps.bind(py).try_iter().ok())
        .map(|iter| iter.flatten().map(Bound::unbind).collect())
        .unwrap_or_default()
}

/// The scopes a `SecurityScopes` parameter receives: the parent scopes, then each own scope not already listed, as FastAPI's `_get_oauth_scopes`.
fn oauth_scopes(parent_scopes: &[String], own_scopes: &[String]) -> Vec<String> {
    let mut scopes = parent_scopes.to_vec();
    for scope in own_scopes {
        if !scopes.contains(scope) {
            scopes.push(scope.clone());
        }
    }
    scopes
}

/// Where a callable sits in the dependency tree of an endpoint.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Depth {
    /// The endpoint itself, whose dependencies are top-level.
    Endpoint,
    /// A dependency the endpoint receives, or a dependency of the route.
    TopLevel,
    /// A sub-dependency of another dependency.
    Nested,
}

impl Depth {
    const fn child(self) -> Self {
        match self {
            Self::Endpoint => Self::TopLevel,
            Self::TopLevel | Self::Nested => Self::Nested,
        }
    }
}

/// Flattens `func` and its sub-dependencies into `flat_plan`, and returns its index and whether it uses scopes.
fn extract_and_flatten(
    py: Python,
    keys: &ParserKeys<'_>,
    func: &Bound<PyAny>,
    path_param_names: &[String],
    depth: Depth,
    parent_param_name: Option<&str>,
    parent_scopes: &[String],
    own_scopes: &[String],
    options: MarkerOptions,
    flat_plan: &mut Vec<DependencyNode>,
    visited: &mut Visited,
) -> PyResult<(usize, bool)> {
    let use_cache = options.use_cache;
    let is_top_level = depth != Depth::Nested;
    let func_id = func.as_ptr() as u64;
    let security_scopes = oauth_scopes(parent_scopes, own_scopes);
    let is_async_generator = params::CallableKind::AsyncGenerator.matches(func)?;
    let is_generator = !is_async_generator && params::CallableKind::Generator.matches(func)?;
    let scope = options
        .scope
        .or_else(|| (is_generator || is_async_generator).then_some(DependencyScope::Request));
    let cache_key = |uses_scopes: bool| {
        CacheKey::new(
            func_id,
            uses_scopes.then_some(security_scopes.as_slice()),
            scope,
        )
    };

    if let Some(&calls_use_scopes) = visited.uses_scopes.get(&func_id) {
        let uses_scopes = calls_use_scopes || !own_scopes.is_empty();
        if use_cache && let Some(&idx) = visited.nodes.get(&cache_key(uses_scopes)) {
            if is_top_level {
                let node = &mut flat_plan[idx];
                node.is_top_level = true;
                node.bindings
                    .extend(parent_param_name.map(|name| PyString::intern(py, name).unbind()));
            }
            return Ok((idx, uses_scopes));
        }
    }

    let current_scopes = [parent_scopes, own_scopes].concat();
    let signature = params::resolved_signature(func)?;
    let parameters = signature.getattr(keys.parameters)?;
    let mut sub_deps_use_scopes = false;

    let mut sub_deps: SmallVec<[(String, usize); 4]> = SmallVec::new();
    let items = parameters.call_method0(keys.items)?;

    for item in items.try_iter()? {
        let pair = item?.cast_into::<PyTuple>()?;
        let param_name = pair.get_item(0)?;
        let param_obj = pair.get_item(1)?;
        let param_name_str: &str = param_name.cast::<PyString>()?.to_str()?;

        if param_name_str == "return" {
            continue;
        }

        if let Ok(default) = param_obj.getattr(keys.default) {
            let annotated = params::find_annotated_dependency_marker(&param_obj);
            if annotated.is_some() && params::is_dependency_marker(&default) {
                return Err(PyAssertionError::new_err(format!(
                    "Cannot specify `Depends` in `Annotated` and default value together for {}",
                    param_name.repr()?
                )));
            }
            let marker =
                annotated.or_else(|| (!params::is_inspect_empty(py, &default)).then_some(default));

            let Some(marker) = marker else {
                continue;
            };

            if !params::is_dependency_marker(&marker) {
                continue;
            }

            let target_callable = if let Ok(dep) = marker.getattr(keys.dependency) {
                if dep.is_none() {
                    if let Ok(annotation) = param_obj.getattr(keys.annotation) {
                        params::base_annotation(&annotation)
                    } else {
                        continue;
                    }
                } else {
                    dep
                }
            } else {
                continue;
            };

            let child_own_scopes = marker
                .getattr(keys.scopes)
                .ok()
                .and_then(|value| extract_string_list(&value))
                .unwrap_or_default();

            let (target_index, child_uses_scopes) = extract_and_flatten(
                py,
                keys,
                &target_callable,
                path_param_names,
                depth.child(),
                Some(param_name_str),
                &current_scopes,
                &child_own_scopes,
                MarkerOptions::of(&marker, keys),
                flat_plan,
                visited,
            )?;
            sub_deps_use_scopes |= child_uses_scopes;

            sub_deps.push((param_name_str.to_owned(), target_index));
        }
    }

    let (injection_plan, needs_request_object) =
        build_injection_plan(py, &parameters, path_param_names, &sub_deps, keys)?;
    let calls_use_scopes = sub_deps_use_scopes
        || injection_plan
            .iter()
            .any(|(_, injection)| matches!(injection, InjectionType::SecurityScopes))
        || describe_scheme(py, func).is_some();
    visited.uses_scopes.insert(func_id, calls_use_scopes);
    let uses_scopes = calls_use_scopes || !own_scopes.is_empty();
    let injection_plan = injection_plan
        .into_iter()
        .map(|(name, injection)| (PyString::intern(py, &name).unbind(), injection))
        .collect();
    let is_async = params::CallableKind::Coroutine.matches(func)?;

    let node_index = flat_plan.len();
    flat_plan.push(DependencyNode {
        cache_key: cache_key(uses_scopes),
        func: func.as_unbound().clone(),
        call: func.as_unbound().clone(),
        is_async,
        is_generator,
        is_async_generator,
        index: node_index,
        bindings: parent_param_name
            .filter(|_| is_top_level)
            .map(|name| PyString::intern(py, name).unbind())
            .into_iter()
            .collect(),
        scopes: security_scopes.into(),
        use_cache,
        function_scope: options.scope == Some(DependencyScope::Function),
        is_top_level,
        injection_plan,
        needs_request_object,
        context_factory: Arc::new(OnceLock::new()),
        call_layout: None,
    });

    visited
        .nodes
        .entry(flat_plan[node_index].cache_key.clone())
        .or_insert(node_index);

    Ok((node_index, uses_scopes))
}

fn build_injection_plan(
    py: Python,
    parameters: &Bound<PyAny>,
    path_param_names: &[String],
    sub_deps: &[(String, usize)],
    keys: &ParserKeys<'_>,
) -> PyResult<(Vec<(String, InjectionType)>, bool)> {
    let mut plan = Vec::new();
    let mut needs_request_object = false;
    let parameters = parameters.call_method0(keys.items)?;

    for item in parameters.try_iter()? {
        let pair = item?.cast_into::<PyTuple>()?;
        let key = pair.get_item(0)?;
        let name: String = key.extract()?;
        let param = pair.get_item(1)?;

        if name == "return" {
            continue;
        }

        if let Some((_, target_idx)) = sub_deps.iter().find(|(param_name, _)| param_name == &name) {
            plan.push((name, InjectionType::Dependency(*target_idx)));
            continue;
        }

        let special_injection = param.getattr(keys.annotation).ok().and_then(|ann| {
            if is_connection_annotation(py, &ann) {
                return Some(InjectionType::Request);
            }
            if is_response_annotation(py, &ann) {
                return Some(InjectionType::Response);
            }
            (params::injected_source(py, &ann) == Some(ParameterSource::SecurityScopes))
                .then_some(InjectionType::SecurityScopes)
        });

        if special_injection.is_some() {
            params::assert_no_field_info(py, &param.getattr(keys.annotation)?)?;
        }
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

fn dependency_args(
    py: Python<'_>,
    dep: &DependencyNode,
    results_registry: &[Option<SharedPyObject>],
    request_input: &RequestInput<'_>,
    request: Option<&SharedPyObject>,
) -> PyResult<CallArgs> {
    let mut final_kwargs = CallArgs::new();
    let mut errors = Errors::new(py);

    for (arg_name, injection_type) in &dep.injection_plan {
        let py_arg_name = arg_name.bind(py);
        match injection_type {
            InjectionType::Dependency(target_idx) => {
                if let Some(cached_val) = &results_registry[*target_idx] {
                    final_kwargs.set(py_arg_name, cached_val.bind(py))?;
                }
            }
            InjectionType::Parameter(parameter) => {
                if let Some(value) = request_param_value(py, parameter, request_input, &mut errors)?
                {
                    final_kwargs.set(py_arg_name, value)?;
                }
            }
            InjectionType::Request => {
                if let Some(req) = request {
                    final_kwargs.set(py_arg_name, req.bind(py))?;
                }
            }
            InjectionType::SecurityScopes => {
                let py_scopes = Py::new(
                    py,
                    PySecurityScopes {
                        scopes: dep.scopes.clone(),
                    },
                )?;
                final_kwargs.set(py_arg_name, py_scopes)?;
            }
            InjectionType::Response => {}
        }
    }

    errors.check(|| Ok(None))?;
    Ok(final_kwargs)
}

pub fn execute_dependencies_sync(
    py: Python<'_>,
    flat_plan: &[DependencyNode],
    request_input: &RequestInput<'_>,
    request: Option<Py<PyAny>>,
) -> PyResult<(ResolvedDependencies, TeardownTasks)> {
    let request = request;
    let mut results_registry: ResultRegistry = smallvec::smallvec![None; flat_plan.len()];
    let mut teardown_tasks = TeardownTasks::new();

    let mut final_results = ResolvedDependencies::new();

    for dep in flat_plan {
        let (result, teardown) =
            execute_sync_node(py, dep, &results_registry, request_input, request.as_ref())?;
        if let Some(teardown) = teardown {
            teardown_tasks.push(teardown);
        }

        bind_result(py, dep, &result, &mut final_results);
        results_registry[dep.index] = Some(result);
    }

    Ok((final_results, teardown_tasks))
}

pub async fn execute_dependencies(
    event_loop: EventLoop,
    flat_plan: &[DependencyNode],
    request_input: &RequestInput<'_>,
    request: Option<Py<PyAny>>,
) -> PyResult<(ResolvedDependencies, TeardownTasks)> {
    let mut teardowns = TeardownTasks::new();
    match execute_dependencies_inner(
        event_loop,
        flat_plan,
        request_input,
        request,
        &mut teardowns,
    )
    .await
    {
        Ok(results) => Ok((results, teardowns)),
        Err(error) => {
            execute_teardowns_async(event_loop, teardowns, Some(&error)).await;
            Err(error)
        }
    }
}

async fn execute_dependencies_inner(
    event_loop: EventLoop,
    flat_plan: &[DependencyNode],
    request_input: &RequestInput<'_>,
    request: Option<Py<PyAny>>,
    teardown_tasks: &mut TeardownTasks,
) -> PyResult<ResolvedDependencies> {
    let mut results_registry: ResultRegistry = smallvec::smallvec![None; flat_plan.len()];
    let mut final_results = ResolvedDependencies::new();

    let mut idx = 0;
    while idx < flat_plan.len() {
        let dep = &flat_plan[idx];

        if dep.is_sync_callable() {
            let run_end = idx
                + flat_plan[idx..]
                    .iter()
                    .take_while(|node| node.is_sync_callable())
                    .count();

            let batch = &flat_plan[idx..run_end];
            Python::attach(|py| -> PyResult<()> {
                let mut run = || -> PyResult<()> {
                    for node in batch {
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
                        register_result(
                            py,
                            node,
                            result,
                            &mut results_registry,
                            &mut final_results,
                        );
                    }
                    Ok(())
                };
                if batch.iter().any(|node| runs_python_code(py, node)) {
                    offload_sync(run)
                } else {
                    run()
                }
            })?;

            idx = run_end;
            continue;
        }

        if dep.is_async_generator {
            let future = Python::attach(|py| -> PyResult<_> {
                let generator =
                    call_dependency(py, dep, &results_registry, request_input, request.as_ref())?;

                let anext_coroutine = generator.call_method0(intern!(py, "__anext__"))?;

                let future = py_bridge::schedule_task(py, event_loop, anext_coroutine)?;
                Ok((generator.unbind(), future))
            })?;

            let (generator, future_anext) = future;
            let outcome = future_anext.await;
            Python::attach(|py| -> PyResult<()> {
                let value = match outcome {
                    Ok(v) => v,
                    Err(e) => {
                        return Err(if e.is_instance_of::<PyStopAsyncIteration>(py) {
                            PyRuntimeError::new_err(
                                "Async generator dependency exited before yielding",
                            )
                        } else {
                            e
                        });
                    }
                };
                teardown_tasks.push(TeardownTask {
                    generator: generator.clone_ref(py),
                    is_async: true,
                    function_scope: dep.function_scope,
                });
                register_result(py, dep, value, &mut results_registry, &mut final_results);
                Ok(())
            })?;
        } else {
            let future = Python::attach(|py| -> PyResult<_> {
                let coroutine =
                    call_dependency(py, dep, &results_registry, request_input, request.as_ref())?;
                py_bridge::schedule_task(py, event_loop, coroutine)
            })?;
            let outcome = future.await;

            Python::attach(|py| -> PyResult<()> {
                let result = outcome?;
                register_result(py, dep, result, &mut results_registry, &mut final_results);
                Ok(())
            })?;
        }

        idx += 1;
    }

    Ok(final_results)
}

/// Whether a sync dependency runs Python code that could block, rather than a native security
/// scheme's `_resolve`.
#[inline]
fn runs_python_code(py: Python<'_>, node: &DependencyNode) -> bool {
    !node.call.bind(py).is_instance_of::<PyCFunction>()
}

/// Runs sync dependency code off the async scheduler, as FastAPI runs a sync dependency of an
/// async endpoint with `run_in_threadpool`: tokio hands this worker's other tasks to another
/// thread while `run` blocks. Outside a multi-thread runtime it runs `run` in place.
#[inline]
pub fn offload_sync<R>(run: impl FnOnce() -> R) -> R {
    match Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == RuntimeFlavor::MultiThread => block_in_place(run),
        _ => run(),
    }
}

#[inline]
fn register_result(
    py: Python<'_>,
    dep: &DependencyNode,
    result: SharedPyObject,
    results_registry: &mut [Option<SharedPyObject>],
    final_results: &mut ResolvedDependencies,
) {
    bind_result(py, dep, &result, final_results);
    results_registry[dep.index] = Some(result);
}

#[inline]
fn bind_result(
    py: Python<'_>,
    dep: &DependencyNode,
    result: &SharedPyObject,
    final_results: &mut ResolvedDependencies,
) {
    final_results.extend(
        dep.bindings
            .iter()
            .map(|name| (name.clone_ref(py), result.clone_ref(py))),
    );
}

#[inline]
fn execute_sync_node(
    py: Python<'_>,
    dep: &DependencyNode,
    results_registry: &[Option<SharedPyObject>],
    request_input: &RequestInput<'_>,
    request: Option<&SharedPyObject>,
) -> PyResult<(SharedPyObject, Option<TeardownTask>)> {
    let _entered = py_bridge::enter_request_copy(py)?;
    if dep.is_generator {
        let generator = call_dependency(py, dep, results_registry, request_input, request)?;
        let value = match generator.call_method0(intern!(py, "__next__")) {
            Ok(v) => v.unbind(),
            Err(e) => {
                return Err(if e.is_instance_of::<PyStopIteration>(py) {
                    PyRuntimeError::new_err("Generator dependency exited before yielding")
                } else {
                    e
                });
            }
        };
        let teardown = TeardownTask {
            generator: generator.unbind(),
            is_async: false,
            function_scope: dep.function_scope,
        };
        Ok((value, Some(teardown)))
    } else {
        Ok((
            call_dependency(py, dep, results_registry, request_input, request)?.unbind(),
            None,
        ))
    }
}

fn call_dependency<'py>(
    py: Python<'py>,
    dep: &DependencyNode,
    results_registry: &[Option<SharedPyObject>],
    request_input: &RequestInput<'_>,
    request: Option<&SharedPyObject>,
) -> PyResult<Bound<'py, PyAny>> {
    let func = dep.call.bind(py);
    if dep.injection_plan.is_empty() {
        return func.call0();
    }
    let args = dependency_args(py, dep, results_registry, request_input, request)?;
    args.call(func, dep.call_layout.as_deref())
}

/// Finishes the yield dependencies of a sync route and logs a teardown failure, for callers
/// with no request left to fail. See `run_teardowns_sync`.
#[inline(always)]
pub fn execute_teardowns_sync(py: Python<'_>, teardowns: TeardownTasks, error: Option<&PyErr>) {
    if teardowns.is_empty() {
        return;
    }
    if let Err(raised) = run_teardowns_sync(py, teardowns, error) {
        log_teardown_error(py, &raised);
    }
}

/// Finishes the yield dependencies in reverse order, as `ExitStack.__exit__` does in FastAPI:
/// each one sees the exception in flight at its `yield`, a new exception raised by a teardown
/// replaces it for the teardowns after it, and that new exception is returned. Re-raising the
/// exception that was thrown in is not a new failure.
pub fn run_teardowns_sync(
    py: Python<'_>,
    teardowns: TeardownTasks,
    error: Option<&PyErr>,
) -> PyResult<()> {
    let mut raised: Option<PyErr> = None;
    for task in teardowns.into_iter().rev() {
        let in_flight = raised.as_ref().or(error);
        let outcome = if task.is_async {
            close_async_from_sync(py, task.generator.bind(py), in_flight)
        } else {
            py_bridge::enter_request_copy(py)
                .and_then(|_entered| finish_sync_generator(py, task.generator.bind(py), in_flight))
        };
        if let Err(new) = outcome {
            raised = Some(new);
        }
    }
    raised.map_or(Ok(()), Err)
}

/// Finishes the yield dependencies of an async route or a websocket and logs a teardown failure,
/// as uvicorn logs an exception that escapes the application. See `run_teardowns_async`.
pub async fn execute_teardowns_async(
    event_loop: EventLoop,
    teardowns: TeardownTasks,
    error: Option<&PyErr>,
) {
    if teardowns.is_empty() {
        return;
    }
    if let Err(raised) = run_teardowns_async(event_loop, teardowns, error).await {
        Python::attach(|py| log_teardown_error(py, &raised));
    }
}

/// `run_teardowns_sync` for an async route: async generators are resumed on the event loop and
/// sync ones on the blocking pool, as FastAPI runs them through `contextmanager_in_threadpool`.
pub async fn run_teardowns_async(
    event_loop: EventLoop,
    teardowns: TeardownTasks,
    error: Option<&PyErr>,
) -> PyResult<()> {
    let mut raised: Option<PyErr> = None;
    let mut sync_batch: Vec<Py<PyAny>> = Vec::new();

    for task in teardowns.into_iter().rev() {
        if task.is_async {
            flush_sync_teardowns(&mut sync_batch, error, &mut raised).await;
            if let Err(new) =
                finish_async_generator(event_loop, task.generator, raised.as_ref().or(error)).await
            {
                raised = Some(new);
            }
        } else {
            sync_batch.push(task.generator);
        }
    }
    flush_sync_teardowns(&mut sync_batch, error, &mut raised).await;
    raised.map_or(Ok(()), Err)
}

#[cold]
fn log_teardown_error(py: Python<'_>, raised: &PyErr) {
    tracing::error!("Exception in generator dependency teardown: {raised}");
    raised.display(py);
}

async fn flush_sync_teardowns(
    batch: &mut Vec<Py<PyAny>>,
    error: Option<&PyErr>,
    raised: &mut Option<PyErr>,
) {
    if batch.is_empty() {
        return;
    }
    let generators = take(batch);
    let in_flight = Python::attach(|py| raised.as_ref().or(error).map(|error| error.clone_ref(py)));
    let outcome = blocking::run_python(move |py| {
        let mut new_raised: Option<PyErr> = None;
        for generator in generators {
            let current = new_raised.as_ref().or(in_flight.as_ref());
            if let Err(new) = finish_sync_generator(py, generator.bind(py), current) {
                new_raised = Some(new);
            }
        }
        new_raised
    })
    .await;
    match outcome {
        Ok(Some(new)) => *raised = Some(new),
        Ok(None) => {}
        Err(panicked) => *raised = Some(PyRuntimeError::new_err(panicked.to_string())),
    }
}

/// Resumes one sync generator at its `yield`, as `contextmanager.__exit__` does.
fn finish_sync_generator(
    py: Python<'_>,
    generator: &Bound<'_, PyAny>,
    error: Option<&PyErr>,
) -> PyResult<()> {
    let finished = match error {
        Some(error) => generator.call_method1(intern!(py, "throw"), (thrown(py, error),)),
        None => generator.call_method0(intern!(py, "__next__")),
    };
    match teardown_outcome::<PyStopIteration>(py, finished.map(Bound::unbind), error) {
        Ok(true) => Ok(()),
        Ok(false) => {
            // It yielded again; close it before reporting, as contextlib does.
            let closed = generator.call_method0(intern!(py, "close"));
            Err(closed.err().unwrap_or_else(|| didnt_stop(error, "throw()")))
        }
        Err(raised) => Err(raised),
    }
}

/// Resumes one async generator at its `yield`, as `asynccontextmanager.__aexit__` does.
async fn finish_async_generator(
    event_loop: EventLoop,
    generator: Py<PyAny>,
    error: Option<&PyErr>,
) -> PyResult<()> {
    let resume = Python::attach(|py| {
        let generator = generator.bind(py);
        let coroutine = match error {
            Some(error) => generator.call_method1(intern!(py, "athrow"), (thrown(py, error),)),
            None => generator.call_method0(intern!(py, "__anext__")),
        }?;
        py_bridge::schedule_task(py, event_loop, coroutine)
    });
    let finished = match resume {
        Ok(future) => future.await,
        Err(raised) => Err(raised),
    };
    let stopped =
        Python::attach(|py| teardown_outcome::<PyStopAsyncIteration>(py, finished, error))?;
    if stopped {
        return Ok(());
    }
    // It yielded again; close it before reporting, as contextlib does.
    let close = Python::attach(|py| {
        let coroutine = generator.bind(py).call_method0(intern!(py, "aclose"))?;
        py_bridge::schedule_task(py, event_loop, coroutine)
    });
    let closed = match close {
        Ok(future) => future.await.map(drop),
        Err(raised) => Err(raised),
    };
    Err(closed
        .err()
        .unwrap_or_else(|| didnt_stop(error, "athrow()")))
}

/// A sync route cannot await an async generator's teardown, so it is handed to the server's
/// event loop to finish there.
fn close_async_from_sync(
    py: Python<'_>,
    generator: &Bound<'_, PyAny>,
    error: Option<&PyErr>,
) -> PyResult<()> {
    let coroutine = match error {
        Some(error) => generator.call_method1(intern!(py, "athrow"), (thrown(py, error),))?,
        None => generator.call_method0(intern!(py, "aclose"))?,
    };
    let Some(event_loop) = server_state::current_server() else {
        drop(coroutine.call_method0(intern!(py, "close")));
        return Err(PyRuntimeError::new_err(
            "async generator dependency could not be finished: no running event loop",
        ));
    };
    RUN_COROUTINE_THREADSAFE
        .get(py)?
        .call1((coroutine, event_loop.bind(py)))
        .map(drop)
}

/// The exception thrown into a generator, carrying its traceback.
fn thrown<'py>(py: Python<'py>, error: &PyErr) -> Bound<'py, PyBaseException> {
    error.clone_ref(py).into_value(py).into_bound(py)
}

#[cold]
fn didnt_stop(error: Option<&PyErr>, after: &str) -> PyErr {
    PyRuntimeError::new_err(match error {
        Some(_) => format!("generator didn't stop after {after}"),
        None => "generator didn't stop".to_owned(),
    })
}

/// `Ok(true)` when the generator finished, `Ok(false)` when it yielded again, and the error when
/// it raised something other than the exception that was thrown into it.
fn teardown_outcome<Done: pyo3::PyTypeInfo>(
    py: Python<'_>,
    finished: PyResult<Py<PyAny>>,
    error: Option<&PyErr>,
) -> PyResult<bool> {
    match finished {
        Ok(_) => Ok(false),
        Err(raised) if raised.is_instance_of::<Done>(py) => Ok(true),
        Err(raised) if error.is_some_and(|error| raised.value(py).is(error.value(py))) => Ok(true),
        Err(raised) => Err(raised),
    }
}
