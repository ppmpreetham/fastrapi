//! The flattening of a tree of `APIRouter` objects into one list of routes.
//! It applies the prefixes and the dependencies of the parent routers to each route.

use crate::ffi::decorators::PyAPIRouter;
use fastrapi_core::path::{extract_path_param_names, join_path};
use fastrapi_http::response_type::ResponseType;
use fastrapi_openapi::document::deep_merge_json;
use fastrapi_openapi::paths::parse_callbacks_to_json;
use fastrapi_routing::analysis::get_response_type_from_class;
use fastrapi_routing::dependencies::{
    DependencyNode, collect_dependency_markers, merge_dependencies, parse_external_dependencies,
};
use fastrapi_routing::route::{RouteEntry, RouteHandler, WebSocketEntry};
use pyo3::Py;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use simd_json::OwnedValue as JsonValue;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::mem::take;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) fn ensure_unfrozen(frozen: &AtomicBool) -> PyResult<()> {
    if frozen.load(Ordering::Acquire) {
        return Err(PyRuntimeError::new_err(
            "Cannot modify router after it has been frozen",
        ));
    }
    Ok(())
}

fn merge_responses(py: Python<'_>, layers: &[Option<&Py<PyAny>>]) -> Option<Py<PyAny>> {
    let dicts: Vec<Bound<'_, PyDict>> = layers
        .iter()
        .flatten()
        .filter_map(|layer| layer.bind(py).cast::<PyDict>().ok().cloned())
        .filter(|dict| !dict.is_empty())
        .collect();
    match dicts.as_slice() {
        [] => None,
        [single] => Some(single.clone().into_any().unbind()),
        _ => {
            let merged = PyDict::new(py);
            for dict in &dicts {
                merged.update(dict.as_mapping()).ok()?;
            }
            Some(merged.into_any().unbind())
        }
    }
}

/// Fills each `{name:kind}` of `template` with the value that Starlette's convertor for `kind` renders.
fn fill_path(py: Python<'_>, template: &str, params: &Bound<'_, PyDict>) -> PyResult<String> {
    let convertors = py
        .import("starlette.convertors")?
        .getattr("CONVERTOR_TYPES")?;
    let mut filled = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let Some(len) = rest[start..].find('}') else {
            break;
        };
        filled.push_str(&rest[..start]);
        let inner = &rest[start + 1..start + len];
        let (key, kind) = inner.split_once(':').unwrap_or((inner, "str"));
        if let Some(value) = params.get_item(key)? {
            let convertor = convertors.get_item(kind)?;
            filled.push_str(
                &convertor
                    .call_method1("to_string", (value,))?
                    .extract::<String>()?,
            );
        }
        rest = &rest[start + len + 1..];
    }
    filled.push_str(rest);
    Ok(filled)
}

impl PyAPIRouter {
    pub(crate) fn url_path_for_name(
        &self,
        py: Python<'_>,
        name: &str,
        path_params: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let path_params = path_params.cloned().unwrap_or_else(|| PyDict::new(py));
        let mut params = HashMap::new();
        for (key, value) in path_params.iter() {
            params.insert(key.extract::<String>()?, value.str()?.to_string());
        }
        let mut stack = vec![(self.clone(), String::new())];
        while let Some((router, prefix)) = stack.pop() {
            let full_prefix = join_path(&prefix, &router.prefix);
            let mut candidates: Vec<(String, &str)> = router
                .route_entries
                .lock()
                .iter()
                .filter(|entry| entry.name == name)
                .map(|entry| (entry.path.clone(), "http"))
                .collect();
            candidates.extend(
                router
                    .websocket_entries
                    .lock()
                    .iter()
                    .filter(|entry| entry.name == name)
                    .map(|entry| (entry.path.clone(), "websocket")),
            );
            for (path, protocol) in candidates {
                let template = join_path(&full_prefix, &path);
                let expected = extract_path_param_names(&template);
                if expected.len() == params.len()
                    && expected.iter().all(|param| params.contains_key(param))
                {
                    let url_path = py
                        .import("starlette.datastructures")?
                        .getattr("URLPath")?
                        .call1((fill_path(py, &template, &path_params)?, protocol))?;
                    return Ok(url_path.unbind());
                }
            }
            for sub in router.sub_routers.lock().iter() {
                let sub_router = sub.router.bind(py).borrow().clone();
                stack.push((sub_router, join_path(&full_prefix, &sub.prefix)));
            }
        }
        let no_match = py.import("starlette.routing")?.getattr("NoMatchFound")?;
        let params_dict = PyDict::new(py);
        for (key, value) in &params {
            params_dict.set_item(key, value)?;
        }
        Err(PyErr::from_value(no_match.call1((name, params_dict))?))
    }

    pub fn mark_frozen(&self) {
        self.frozen.store(true, Ordering::Release);
    }

    pub(crate) fn ensure_mutable(&self) -> PyResult<()> {
        ensure_unfrozen(&self.frozen)
    }

    pub(crate) fn touch(&self) {
        self.revision.fetch_add(1, Ordering::Release);
    }

    pub(crate) fn tree_revision(&self, py: Python<'_>) -> u64 {
        let mut total = self.revision.load(Ordering::Acquire);
        for sub in self.sub_routers.lock().iter() {
            if let Ok(router) = sub.router.bind(py).try_borrow() {
                total = total
                    .wrapping_mul(31)
                    .wrapping_add(router.tree_revision(py));
            }
        }
        total
    }
    pub fn freeze(&self, py: Python<'_>) -> PyResult<()> {
        if self.frozen.load(Ordering::Acquire) && self.cached_flat.lock().is_some() {
            return Ok(());
        }
        let flat = Arc::new(flatten_router(py, self, true)?);
        *self.cached_flat.lock() = Some(flat);
        self.mark_frozen();
        Ok(())
    }

    pub fn flatten(&self, py: Python<'_>) -> PyResult<Arc<(Vec<RouteEntry>, Vec<WebSocketEntry>)>> {
        if !self.frozen.load(Ordering::Acquire) {
            return flatten_router(py, self, false).map(Arc::new);
        }
        if let Some(cached) = self.cached_flat.lock().as_ref() {
            return Ok(cached.clone());
        }
        let flat = Arc::new(flatten_router(py, self, false)?);
        *self.cached_flat.lock() = Some(flat.clone());
        Ok(flat)
    }

    /// The routes in `_low_priority_routes`, as entries of this router.
    fn low_priority_entries(&self, py: Python<'_>) -> PyResult<Vec<RouteEntry>> {
        let routes = self.low_priority_routes.bind(py);
        if routes.is_empty() {
            return Ok(Vec::new());
        }
        let router = Self::default();
        crate::ffi::openapi::add_route_objects(py, &router, routes.as_any())?;
        Ok(take(&mut *router.route_entries.lock()))
    }

    pub fn snapshot(&self, py: Python<'_>) -> PyResult<(Vec<RouteEntry>, Vec<WebSocketEntry>)> {
        flatten_router(py, self, false)
    }
}

type ChainDependencies = Vec<Py<pyo3::PyAny>>;

#[must_use]
struct Level {
    router: PyAPIRouter,
    prefix: String,
    tags: Vec<String>,
    dependencies: ChainDependencies,
    unique_id: Option<Py<pyo3::PyAny>>,
    responses: Option<Py<pyo3::PyAny>>,
    callbacks: Option<JsonValue>,
    include_in_schema: bool,
    deprecated: bool,
    response_class: Option<Py<pyo3::PyAny>>,
    strict_content_type: Option<bool>,
}

fn with_strict_content_type(handler: &Arc<RouteHandler>, strict: bool) -> Arc<RouteHandler> {
    let mut updated = (**handler).clone();
    updated.payload.strict_content_type = Some(strict);
    Arc::new(updated)
}

/// How a route renders plain return values with `class` as its `response_class`.
pub(crate) fn response_class_type(py: Python<'_>, class: &Bound<'_, PyAny>) -> ResponseType {
    match get_response_type_from_class(py, class) {
        ResponseType::Auto => ResponseType::Custom,
        builtin => builtin,
    }
}

fn with_response_class(
    py: Python<'_>,
    handler: &Arc<RouteHandler>,
    class: &Py<pyo3::PyAny>,
) -> Arc<RouteHandler> {
    let mut updated = (**handler).clone();
    updated.response.response_type = response_class_type(py, class.bind(py));
    updated.response.response_class = Some(class.clone_ref(py));
    Arc::new(updated)
}

fn merge_callbacks(base: Option<&JsonValue>, extra: Option<&JsonValue>) -> Option<JsonValue> {
    match (base, extra) {
        (None, None) => None,
        (Some(only), None) | (None, Some(only)) => Some(only.clone()),
        (Some(base), Some(extra)) => {
            let mut merged = base.clone();
            deep_merge_json(&mut merged, extra.clone());
            Some(merged)
        }
    }
}

pub fn validate_dependencies(py: Python<'_>, dependencies: Option<&Py<PyAny>>) -> PyResult<()> {
    let markers = collect_dependency_markers(py, dependencies);
    if !markers.is_empty() {
        parse_external_dependencies(py, &markers, &[])?;
    }
    Ok(())
}

pub fn flatten_router(
    py: Python<'_>,
    root: &PyAPIRouter,
    freeze: bool,
) -> PyResult<(Vec<RouteEntry>, Vec<WebSocketEntry>)> {
    let mut routes = Vec::new();
    let mut low_priority_routes = Vec::new();
    let mut ws_routes = Vec::new();

    let mut stack = Vec::with_capacity(16);
    stack.push(Level {
        router: root.clone(),
        prefix: String::new(),
        tags: Vec::new(),
        dependencies: ChainDependencies::new(),
        unique_id: None,
        responses: None,
        callbacks: None,
        include_in_schema: true,
        deprecated: false,
        response_class: None,
        strict_content_type: None,
    });

    while let Some(level) = stack.pop() {
        let router = level.router;
        let level_responses =
            merge_responses(py, &[level.responses.as_ref(), router.responses.as_ref()]);
        if freeze {
            router.mark_frozen();
        }

        let full_prefix = join_path(&level.prefix, &router.prefix);
        let strict_content_type = router.strict_content_type.or(level.strict_content_type);

        let merged_tags = router.tags.iter().fold(level.tags, |mut acc, tag| {
            if !acc.contains(tag) {
                acc.push(tag.clone());
            }
            acc
        });

        let mut chain_deps = level.dependencies;
        chain_deps.extend(collect_dependency_markers(py, router.dependencies.as_ref()));

        let mut chain_cache: HashMap<Vec<String>, Vec<DependencyNode>> = HashMap::new();
        let chain_security = if chain_deps.is_empty() {
            Vec::new()
        } else {
            super::init::security_requirements(
                py,
                chain_nodes(py, &chain_deps, Vec::new(), &mut chain_cache)?,
            )
        };
        let route_entries = router.route_entries.lock().clone();
        routes.reserve(route_entries.len());
        let low_priority_entries = router.low_priority_entries(py)?;
        let entries = route_entries
            .into_iter()
            .map(|entry| (entry, false))
            .chain(low_priority_entries.into_iter().map(|entry| (entry, true)));
        for (mut entry, low_priority) in entries {
            entry.tags = entry
                .tags
                .into_iter()
                .fold(merged_tags.clone(), |mut acc, tag| {
                    if !acc.contains(&tag) {
                        acc.push(tag);
                    }
                    acc
                });

            entry.path = join_path(&full_prefix, &entry.path);
            entry.responses =
                merge_responses(py, &[level_responses.as_ref(), entry.responses.as_ref()]);
            if entry.generate_unique_id_function.is_none() {
                entry.generate_unique_id_function = level
                    .unique_id
                    .as_ref()
                    .map(|function| function.clone_ref(py));
            }
            entry.include_in_schema &= level.include_in_schema;
            if level.deprecated {
                entry.deprecated = Some(true);
            }
            entry.callbacks = merge_callbacks(level.callbacks.as_ref(), entry.callbacks.as_ref());
            if !entry.explicit_response_class
                && let Some(class) = &level.response_class
            {
                entry.handler = with_response_class(py, &entry.handler, class);
                entry.explicit_response_class = true;
            }

            if entry.handler.payload.strict_content_type.is_none()
                && let Some(strict) = strict_content_type
            {
                entry.handler = with_strict_content_type(&entry.handler, strict);
            }

            if !chain_deps.is_empty() {
                entry.handler =
                    prepend_chain_dependencies(py, &entry.handler, &chain_deps, &mut chain_cache)?;
                let mut security = chain_security.clone();
                security.append(&mut entry.security);
                entry.security = security;
            }
            if low_priority {
                low_priority_routes.push(entry);
            } else {
                routes.push(entry);
            }
        }

        let ws_entries = router.websocket_entries.lock().clone();
        let ws_chain: &[DependencyNode] = if chain_deps.is_empty() || ws_entries.is_empty() {
            &[]
        } else {
            chain_nodes(py, &chain_deps, Vec::new(), &mut chain_cache)?
        };
        ws_routes.extend(ws_entries.into_iter().map(|mut ws| {
            ws.path = join_path(&full_prefix, &ws.path);
            if !ws_chain.is_empty() {
                ws.deps = merge_dependencies(ws_chain.to_vec(), ws.deps.into_vec()).into();
            }
            ws
        }));

        let subs = router.sub_routers.lock().clone();
        for sub in subs {
            let sub_router = sub.router.bind(py).borrow().clone();

            let sub_tags = sub
                .tags
                .into_iter()
                .fold(merged_tags.clone(), |mut acc, tag| {
                    if !acc.contains(&tag) {
                        acc.push(tag);
                    }
                    acc
                });

            let mut sub_deps = chain_deps.clone();
            if let Some(deps) = sub.dependencies.as_ref() {
                sub_deps.extend(collect_dependency_markers(py, Some(deps)));
            }

            let sub_callbacks = sub.callbacks.as_ref().and_then(|callbacks| {
                parse_callbacks_to_json(
                    py,
                    callbacks.bind(py),
                    super::router::collect_router_routes,
                )
            });
            stack.push(Level {
                router: sub_router,
                prefix: join_path(&full_prefix, &sub.prefix),
                tags: sub_tags,
                dependencies: sub_deps,
                unique_id: sub
                    .generate_unique_id_function
                    .as_ref()
                    .or(router.generate_unique_id_function.as_ref())
                    .or(level.unique_id.as_ref())
                    .map(|function| function.clone_ref(py)),
                responses: merge_responses(py, &[level_responses.as_ref(), sub.responses.as_ref()]),
                callbacks: merge_callbacks(level.callbacks.as_ref(), sub_callbacks.as_ref()),
                include_in_schema: level.include_in_schema
                    && router.include_in_schema
                    && sub.include_in_schema,
                deprecated: level.deprecated
                    || sub.deprecated == Some(true)
                    || router.deprecated == Some(true),
                response_class: sub
                    .default_response_class
                    .as_ref()
                    .or(router.default_response_class.as_ref())
                    .or(level.response_class.as_ref())
                    .map(|class| class.clone_ref(py)),
                strict_content_type,
            });
        }
    }

    // Like FastAPI's `_low_priority_routes`, they are tried after every other route.
    routes.append(&mut low_priority_routes);
    Ok((routes, ws_routes))
}

fn chain_nodes<'c>(
    py: Python<'_>,
    chain: &[Py<pyo3::PyAny>],
    path_param_names: Vec<String>,
    cache: &'c mut HashMap<Vec<String>, Vec<DependencyNode>>,
) -> PyResult<&'c [DependencyNode]> {
    let nodes = match cache.entry(path_param_names) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) => {
            let nodes = parse_external_dependencies(py, chain, entry.key())?;
            entry.insert(nodes)
        }
    };
    Ok(nodes.as_slice())
}

fn prepend_chain_dependencies(
    py: Python<'_>,
    handler: &Arc<RouteHandler>,
    chain: &[Py<pyo3::PyAny>],
    cache: &mut HashMap<Vec<String>, Vec<DependencyNode>>,
) -> PyResult<Arc<RouteHandler>> {
    let path_param_names: Vec<String> = handler
        .payload
        .path_param_names
        .iter()
        .map(ToString::to_string)
        .collect();

    let externals = chain_nodes(py, chain, path_param_names, cache)?;
    if externals.is_empty() {
        return Ok(handler.clone());
    }

    let mut merged = <RouteHandler>::clone(handler);
    merged.payload.dependencies =
        merge_dependencies(externals.to_vec(), merged.payload.dependencies);

    merged.payload.needs_kwargs = true;
    merged.payload.refresh_dependency_flags();
    merged.assign_execution_mode();

    Ok(Arc::new(merged))
}
