use crate::ffi::decorators::PyAPIRouter;
use crate::routing::types::{RouteEntry, WebSocketEntry};
use pyo3::Py;
use pyo3::prelude::Python;
use std::sync::Arc;
use std::sync::atomic::Ordering;

impl PyAPIRouter {
    pub fn mark_frozen(&self) {
        self.frozen.store(true, Ordering::Release);
    }
    pub fn freeze(&self, py: Python<'_>) {
        if self.frozen.load(Ordering::Acquire) {
            return;
        }
        let flat = Arc::new(flatten_router(py, self));
        *self.cached_flat.lock() = Some(flat);
        self.mark_frozen();
    }

    pub fn flatten(&self, py: Python<'_>) -> Arc<(Vec<RouteEntry>, Vec<WebSocketEntry>)> {
        if self.frozen.load(Ordering::Acquire) {
            if let Some(cached) = self.cached_flat.lock().as_ref() {
                return cached.clone();
            }
            return Arc::new(flatten_router(py, self));
        }

        Arc::new(flatten_router(py, self))
    }
}

type ChainDependencies = Vec<Py<pyo3::PyAny>>;

pub fn flatten_router(
    py: Python<'_>,
    root: &PyAPIRouter,
) -> (Vec<RouteEntry>, Vec<WebSocketEntry>) {
    let mut routes = Vec::new();
    let mut ws_routes = Vec::new();

    let mut stack = Vec::with_capacity(16);
    let overrides = crate::globals::take_dependency_overrides();
    stack.push((
        root.clone(),
        String::new(),
        Vec::<String>::new(),
        ChainDependencies::new(),
    ));

    while let Some((router, prefix, parent_tags, inherited_deps)) = stack.pop() {
        router.mark_frozen();

        let full_prefix = join_path(&prefix, &router.prefix);

        let merged_tags = router.tags.iter().fold(parent_tags, |mut acc, tag| {
            if !acc.contains(tag) {
                acc.push(tag.clone());
            }
            acc
        });

        let mut chain_deps = inherited_deps;
        chain_deps.extend(crate::routing::dependencies::collect_dependency_callables(
            py,
            router.dependencies.as_ref(),
        ));

        let route_entries = router.route_entries.lock().clone();
        routes.extend(route_entries.into_iter().map(|mut entry| {
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

            if !chain_deps.is_empty() {
                entry.handler = prepend_chain_dependencies(py, &entry.handler, &chain_deps);
            }
            entry.handler = apply_dependency_overrides(py, &entry.handler, &overrides);

            entry
        }));

        let ws_entries = router.websocket_entries.lock().clone();
        ws_routes.extend(ws_entries.into_iter().map(|mut ws| {
            ws.path = join_path(&full_prefix, &ws.path);
            if !chain_deps.is_empty() || !overrides.is_empty() {
                let mut plan = ws.deps.to_vec();
                if !chain_deps.is_empty() {
                    plan = prepend_nodes(py, plan, &parse_chain_nodes(py, &chain_deps, &[]));
                }
                apply_overrides_to_plan(py, &mut plan, &overrides);
                ws.deps = plan.into();
            }
            ws
        }));

        let subs = router.sub_routers.lock().clone();
        stack.extend(subs.into_iter().map(|sub| {
            let sub_router = sub.router.bind(py).borrow();

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
                sub_deps.extend(crate::routing::dependencies::collect_dependency_callables(
                    py,
                    Some(deps),
                ));
            }

            (
                sub_router.clone(),
                join_path(&full_prefix, &sub.prefix),
                sub_tags,
                sub_deps,
            )
        }));
    }

    (routes, ws_routes)
}

fn parse_chain_nodes(
    py: Python<'_>,
    chain: &[Py<pyo3::PyAny>],
    path_param_names: &[String],
) -> Vec<crate::routing::dependencies::DependencyNode> {
    crate::routing::dependencies::parse_external_dependencies(py, chain, path_param_names)
        .unwrap_or_else(|err| {
            tracing::error!("Failed to parse router-level dependencies: {}", err);
            Vec::new()
        })
}

fn prepend_nodes(
    _py: Python<'_>,
    mut base: Vec<crate::routing::dependencies::DependencyNode>,
    externals: &[crate::routing::dependencies::DependencyNode],
) -> Vec<crate::routing::dependencies::DependencyNode> {
    let offset = externals.len();
    if offset == 0 {
        return base;
    }
    crate::routing::dependencies::shift_dependency_indices(&mut base, offset);
    let mut merged = externals.to_vec();
    merged.append(&mut base);
    merged
}

fn apply_overrides_to_plan(
    py: Python<'_>,
    nodes: &mut [crate::routing::dependencies::DependencyNode],
    overrides: &ahash::AHashMap<u64, Py<pyo3::PyAny>>,
) {
    for node in nodes.iter_mut() {
        if let Some(replacement) = overrides.get(&node.func_id) {
            crate::routing::dependencies::override_node_callable(py, node, replacement);
        }
    }
}

fn prepend_chain_dependencies(
    py: Python<'_>,
    handler: &Arc<crate::routing::types::RouteHandler>,
    chain: &[Py<pyo3::PyAny>],
) -> Arc<crate::routing::types::RouteHandler> {
    let path_param_names: Vec<String> = handler
        .payload
        .path_param_names
        .iter()
        .map(|n| n.to_string())
        .collect();

    let externals = parse_chain_nodes(py, chain, &path_param_names);
    if externals.is_empty() {
        return handler.clone();
    }

    let mut merged = <crate::routing::types::RouteHandler>::clone(handler);
    merged.payload.dependencies = prepend_nodes(py, merged.payload.dependencies, &externals);

    merged.payload.needs_kwargs = true;
    merged.payload.dependency_needs_request = merged
        .payload
        .dependencies
        .iter()
        .any(|node| node.needs_request_object);
    merged.payload.all_deps_sync = merged
        .payload
        .dependencies
        .iter()
        .all(|node| node.is_sync_callable());

    crate::ffi::py_handlers::assign_execution_mode(&mut merged);

    Arc::new(merged)
}

fn apply_dependency_overrides(
    py: Python<'_>,
    handler: &Arc<crate::routing::types::RouteHandler>,
    overrides: &ahash::AHashMap<u64, Py<pyo3::PyAny>>,
) -> Arc<crate::routing::types::RouteHandler> {
    if overrides.is_empty()
        || !handler
            .payload
            .dependencies
            .iter()
            .any(|node| overrides.contains_key(&node.func_id))
    {
        return handler.clone();
    }

    let mut merged = <crate::routing::types::RouteHandler>::clone(handler);
    for node in merged.payload.dependencies.iter_mut() {
        if let Some(replacement) = overrides.get(&node.func_id) {
            crate::routing::dependencies::override_node_callable(py, node, replacement);
        }
    }

    merged.payload.dependency_needs_request = merged
        .payload
        .dependencies
        .iter()
        .any(|node| node.needs_request_object);
    merged.payload.all_deps_sync = merged
        .payload
        .dependencies
        .iter()
        .all(|node| node.is_sync_callable());

    crate::ffi::py_handlers::assign_execution_mode(&mut merged);
    Arc::new(merged)
}

pub fn join_path(a: &str, b: &str) -> String {
    let a_ends = a.ends_with('/');
    let b_starts = b.starts_with('/');

    let capacity = match (a_ends, b_starts) {
        (true, true) => a.len() + b.len() - 1,
        (false, false) => a.len() + b.len() + 1,
        _ => a.len() + b.len(),
    };

    let mut path = String::with_capacity(capacity);
    path.push_str(a);

    match (a_ends, b_starts) {
        (true, true) => {
            path.pop();
            path.push_str(b);
        }
        (false, false) => {
            path.push('/');
            path.push_str(b);
        }
        _ => {
            path.push_str(b);
        }
    }
    path
}
