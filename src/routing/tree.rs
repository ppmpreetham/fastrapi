use crate::ffi::decorators::PyAPIRouter;
use crate::ffi::py_handlers::assign_execution_mode;
use crate::routing::dependencies::DependencyNode;
use crate::routing::dependencies::InjectionType;
use crate::routing::dependencies::collect_dependency_callables;
use crate::routing::dependencies::parse_external_dependencies;
use crate::routing::dependencies::shift_dependency_indices;
use crate::routing::types::RouteHandler;
use crate::routing::types::{RouteEntry, WebSocketEntry};
use pyo3::Py;
use pyo3::prelude::Python;
use std::string::ToString;
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

    #[must_use]
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

#[must_use]
pub fn flatten_router(
    py: Python<'_>,
    root: &PyAPIRouter,
) -> (Vec<RouteEntry>, Vec<WebSocketEntry>) {
    let mut routes = Vec::new();
    let mut ws_routes = Vec::new();

    let mut stack = Vec::with_capacity(16);
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
        chain_deps.extend(collect_dependency_callables(
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
            entry
        }));

        let ws_entries = router.websocket_entries.lock().clone();
        ws_routes.extend(ws_entries.into_iter().map(|mut ws| {
            ws.path = join_path(&full_prefix, &ws.path);
            if !chain_deps.is_empty() {
                let mut plan = ws.deps.to_vec();
                if !chain_deps.is_empty() {
                    plan = prepend_nodes(py, plan, &parse_chain_nodes(py, &chain_deps, &[]));
                }
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
                sub_deps.extend(collect_dependency_callables(py, Some(deps)));
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
) -> Vec<DependencyNode> {
    parse_external_dependencies(py, chain, path_param_names).unwrap_or_else(|err| {
        tracing::error!("Failed to parse router-level dependencies: {}", err);
        Vec::new()
    })
}

fn prepend_nodes(
    _py: Python<'_>,
    mut base: Vec<DependencyNode>,
    externals: &[DependencyNode],
) -> Vec<DependencyNode> {
    let offset = externals.len();
    if offset == 0 {
        return base;
    }
    shift_dependency_indices(&mut base, offset);
    let mut merged = externals.to_vec();
    merged.append(&mut base);
    merged
}

fn prepend_chain_dependencies(
    py: Python<'_>,
    handler: &Arc<RouteHandler>,
    chain: &[Py<pyo3::PyAny>],
) -> Arc<RouteHandler> {
    let path_param_names: Vec<String> = handler
        .payload
        .path_param_names
        .iter()
        .map(ToString::to_string)
        .collect();

    let externals = parse_chain_nodes(py, chain, &path_param_names);
    if externals.is_empty() {
        return handler.clone();
    }

    let mut merged = <RouteHandler>::clone(handler);
    merged.payload.dependencies = prepend_nodes(py, merged.payload.dependencies, &externals);

    merged.payload.needs_kwargs = true;
    merged.payload.dependency_needs_request = merged
        .payload
        .dependencies
        .iter()
        .any(|node| node.needs_request_object);
    merged.payload.dependency_needs_response = merged.payload.dependencies.iter().any(|node| {
        node.injection_plan
            .iter()
            .any(|(_, injection)| matches!(injection, InjectionType::Response))
    });
    merged.payload.has_generators = merged
        .payload
        .dependencies
        .iter()
        .any(|node| node.is_generator || node.is_async_generator);
    merged.payload.has_async_dependencies = merged
        .payload
        .dependencies
        .iter()
        .any(|node| node.is_async || node.is_async_generator);
    merged.payload.all_deps_sync = merged
        .payload
        .dependencies
        .iter()
        .all(super::dependencies::DependencyNode::is_sync_callable);

    assign_execution_mode(&mut merged);

    Arc::new(merged)
}

#[must_use]
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
