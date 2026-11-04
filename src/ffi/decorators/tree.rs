//! The flattening of a tree of `APIRouter` objects into one list of routes.
//! It applies the prefixes and the dependencies of the parent routers to each route.

use crate::ffi::decorators::PyAPIRouter;
use fastrapi_core::path::{extract_path_param_names, join_path};
use fastrapi_openapi::document::deep_merge_json;
use fastrapi_openapi::paths::parse_callbacks_to_json;
use fastrapi_routing::analysis::get_response_type_from_class;
use fastrapi_routing::dependencies::{
    DependencyNode, collect_dependency_callables, parse_external_dependencies,
    shift_dependency_indices,
};
use fastrapi_routing::route::{RouteEntry, RouteHandler, WebSocketEntry};
use pyo3::Py;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use simd_json::OwnedValue as JsonValue;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

fn merge_responses(py: Python<'_>, layers: &[Option<&Py<PyAny>>]) -> Option<Py<PyAny>> {
    let dicts: Vec<Bound<'_, PyDict>> = layers
        .iter()
        .flatten()
        .filter_map(|layer| layer.bind(py).cast::<PyDict>().ok().cloned())
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

fn fill_path(template: &str, params: &HashMap<String, String>) -> String {
    let mut filled = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let Some(len) = rest[start..].find('}') else {
            break;
        };
        filled.push_str(&rest[..start]);
        let inner = &rest[start + 1..start + len];
        let key = inner.split(':').next().unwrap_or(inner);
        filled.push_str(params.get(key).map_or("", String::as_str));
        rest = &rest[start + len + 1..];
    }
    filled.push_str(rest);
    filled
}

impl PyAPIRouter {
    pub(crate) fn url_path_for_name(
        &self,
        py: Python<'_>,
        name: &str,
        path_params: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let mut params = HashMap::new();
        if let Some(path_params) = path_params {
            for (key, value) in path_params.iter() {
                params.insert(key.extract::<String>()?, value.str()?.to_string());
            }
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
                        .call1((fill_path(&template, &params), protocol))?;
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
    pub fn freeze(&self, py: Python<'_>) {
        if self.frozen.load(Ordering::Acquire) {
            return;
        }
        let flat = Arc::new(flatten_router(py, self, true));
        *self.cached_flat.lock() = Some(flat);
        self.mark_frozen();
    }

    #[must_use]
    pub fn flatten(&self, py: Python<'_>) -> Arc<(Vec<RouteEntry>, Vec<WebSocketEntry>)> {
        if self.frozen.load(Ordering::Acquire) {
            if let Some(cached) = self.cached_flat.lock().as_ref() {
                return cached.clone();
            }
            return Arc::new(flatten_router(py, self, true));
        }

        Arc::new(flatten_router(py, self, true))
    }

    #[must_use]
    pub fn snapshot(&self, py: Python<'_>) -> (Vec<RouteEntry>, Vec<WebSocketEntry>) {
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
}

fn with_response_class(
    py: Python<'_>,
    handler: &Arc<RouteHandler>,
    class: &Py<pyo3::PyAny>,
) -> Arc<RouteHandler> {
    let response_type = get_response_type_from_class(py, class.bind(py));
    let mut updated = (**handler).clone();
    updated.response.response_type = response_type;
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

#[must_use]
pub fn flatten_router(
    py: Python<'_>,
    root: &PyAPIRouter,
    freeze: bool,
) -> (Vec<RouteEntry>, Vec<WebSocketEntry>) {
    let mut routes = Vec::new();
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
    });

    while let Some(level) = stack.pop() {
        let router = level.router;
        let level_responses =
            merge_responses(py, &[level.responses.as_ref(), router.responses.as_ref()]);
        if freeze {
            router.mark_frozen();
        }

        let full_prefix = join_path(&level.prefix, &router.prefix);

        let merged_tags = router.tags.iter().fold(level.tags, |mut acc, tag| {
            if !acc.contains(tag) {
                acc.push(tag.clone());
            }
            acc
        });

        let mut chain_deps = level.dependencies;
        chain_deps.extend(collect_dependency_callables(
            py,
            router.dependencies.as_ref(),
        ));

        let chain_security = if chain_deps.is_empty() {
            Vec::new()
        } else {
            super::init::security_requirements(py, &parse_chain_nodes(py, &chain_deps, &[]))
        };
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

            if !chain_deps.is_empty() {
                entry.handler = prepend_chain_dependencies(py, &entry.handler, &chain_deps);
                let mut security = chain_security.clone();
                security.append(&mut entry.security);
                entry.security = security;
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
                sub_deps.extend(collect_dependency_callables(py, Some(deps)));
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
            });
        }
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
    merged.payload.refresh_dependency_flags();
    merged.assign_execution_mode();

    Arc::new(merged)
}
