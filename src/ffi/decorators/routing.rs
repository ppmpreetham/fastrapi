use super::PyAPIRouter;
use crate::routing::types::{RouteEntry, WebSocketEntry};
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
        *self.cached_flat.lock().unwrap() = Some(flat);
        self.mark_frozen();
    }

    pub fn flatten(&self, py: Python<'_>) -> Arc<(Vec<RouteEntry>, Vec<WebSocketEntry>)> {
        if self.frozen.load(Ordering::Acquire) {
            if let Some(cached) = self.cached_flat.lock().unwrap().as_ref() {
                return cached.clone();
            }
            return Arc::new(flatten_router(py, self));
        }

        Arc::new(flatten_router(py, self))
    }
}

pub fn flatten_router(py: Python<'_>, root: &PyAPIRouter) -> (Vec<RouteEntry>, Vec<WebSocketEntry>) {
    let mut routes = Vec::new();
    let mut ws_routes = Vec::new();
    let mut stack = vec![(root.clone(), String::new(), Vec::<String>::new())];

    while let Some((router, prefix, parent_tags)) = stack.pop() {
        router.mark_frozen();

        let full_prefix = join_path(&prefix, &router.prefix);

        let mut current_tags = parent_tags;
        for tag in &router.tags {
            if !current_tags.contains(tag) {
                current_tags.push(tag.clone());
            }
        }

        let route_entries = router.route_entries.lock().unwrap().clone();
        routes.extend(route_entries.into_iter().map(|entry| {
            let mut tags = current_tags.clone();
            for tag in &entry.tags {
                if !tags.contains(tag) {
                    tags.push(tag.clone());
                }
            }

            RouteEntry {
                method: entry.method,
                path: join_path(&full_prefix, &entry.path),
                handler: entry.handler,
                tags,
                summary: entry.summary.clone(),
                description: entry.description.clone(),
                response_description: entry.response_description.clone(),
                operation_id: entry.operation_id.clone(),
                responses: entry.responses.clone(),
                openapi_extra: entry.openapi_extra.clone(),
                callbacks: entry.callbacks.clone(),
                deprecated: entry.deprecated,
                include_in_schema: entry.include_in_schema,
            }
        }));

        let ws_entries = router.websocket_entries.lock().unwrap().clone();
        ws_routes.extend(ws_entries.into_iter().map(|ws| WebSocketEntry {
            path: join_path(&full_prefix, &ws.path),
            handler: ws.handler.clone_ref(py),
        }));

        let subs = router.sub_routers.lock().unwrap().clone();
        for sub in subs {
            let sub_router = sub.router.bind(py).borrow();

            let mut sub_tags = current_tags.clone();
            for tag in &sub.tags {
                if !sub_tags.contains(tag) {
                    sub_tags.push(tag.clone());
                }
            }

            stack.push((
                sub_router.clone(),
                join_path(&full_prefix, &sub.prefix),
                sub_tags,
            ));
        }
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
