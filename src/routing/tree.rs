use crate::ffi::decorators::PyAPIRouter;
use crate::routing::types::{RouteEntry, WebSocketEntry};
use crate::utils::LockExt;
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
        *self.cached_flat.lock_or_panic() = Some(flat);
        self.mark_frozen();
    }

    pub fn flatten(&self, py: Python<'_>) -> Arc<(Vec<RouteEntry>, Vec<WebSocketEntry>)> {
        if self.frozen.load(Ordering::Acquire) {
            if let Some(cached) = self.cached_flat.lock_or_panic().as_ref() {
                return cached.clone();
            }
            return Arc::new(flatten_router(py, self));
        }

        Arc::new(flatten_router(py, self))
    }
}

pub fn flatten_router(
    py: Python<'_>,
    root: &PyAPIRouter,
) -> (Vec<RouteEntry>, Vec<WebSocketEntry>) {
    let mut routes = Vec::new();
    let mut ws_routes = Vec::new();

    let mut stack = Vec::with_capacity(16);
    stack.push((root.clone(), String::new(), Vec::<String>::new()));

    while let Some((router, prefix, parent_tags)) = stack.pop() {
        router.mark_frozen();

        let full_prefix = join_path(&prefix, &router.prefix);

        let merged_tags = router.tags.iter().fold(parent_tags, |mut acc, tag| {
            if !acc.contains(tag) {
                acc.push(tag.clone());
            }
            acc
        });

        let route_entries = router.route_entries.lock_or_panic().clone();
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
            entry
        }));

        let ws_entries = router.websocket_entries.lock_or_panic().clone();
        ws_routes.extend(ws_entries.into_iter().map(|mut ws| {
            ws.path = join_path(&full_prefix, &ws.path);
            ws
        }));

        let subs = router.sub_routers.lock_or_panic().clone();
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

            (
                sub_router.clone(),
                join_path(&full_prefix, &sub.prefix),
                sub_tags,
            )
        }));
    }

    (routes, ws_routes)
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
