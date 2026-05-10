use crate::routing::types::{HttpMethod, RouteHandler, HTTP_METHOD_COUNT};
use ahash::AHashMap;
use pyo3::{Py, PyAny};
use std::{borrow::Cow, sync::Arc};

pub enum RouteMatch<'a> {
    Static(Arc<RouteHandler>),
    Params(Arc<RouteHandler>, matchit::Params<'a, 'a>),
}

#[derive(Clone)]
pub struct FrozenRouter {
    static_routes: [AHashMap<Box<str>, Arc<RouteHandler>>; HTTP_METHOD_COUNT],
    param_routes: [Option<matchit::Router<Arc<RouteHandler>>>; HTTP_METHOD_COUNT],
    websocket_routes: AHashMap<String, Py<PyAny>>,
}

impl FrozenRouter {
    #[inline(always)]
    pub fn resolve<'a>(&'a self, method: HttpMethod, path: &'a str) -> Option<RouteMatch<'a>> {
        let idx = method as usize;
        let normalized = normalize_lookup(path);
        if let Some(handler) = self.static_routes[idx].get(normalized.as_ref()) {
            return Some(RouteMatch::Static(handler.clone()));
        }
        let matched = self.param_routes[idx].as_ref()?.at(path).ok()?;
        Some(RouteMatch::Params(matched.value.clone(), matched.params))
    }

    pub fn resolve_ws(&self, path: &str) -> Option<Py<PyAny>> {
        let normalized = normalize_lookup(path);
        self.websocket_routes.get(normalized.as_ref()).cloned()
    }
}

pub struct FrozenRouterBuilder {
    static_routes: [AHashMap<Box<str>, Arc<RouteHandler>>; HTTP_METHOD_COUNT],
    param_entries: [Vec<(String, Arc<RouteHandler>)>; HTTP_METHOD_COUNT],
    websocket_routes: AHashMap<String, Py<PyAny>>,
}

impl FrozenRouterBuilder {
    pub fn new() -> Self {
        Self {
            static_routes: std::array::from_fn(|_| AHashMap::new()),
            param_entries: std::array::from_fn(|_| Vec::new()),
            websocket_routes: AHashMap::new(),
        }
    }

    pub fn add_route(&mut self, method: HttpMethod, path: String, handler: Arc<RouteHandler>) {
        let idx = method as usize;
        let (normalized, has_params) = normalize_register(&path);

        if has_params {
            self.param_entries[idx].push((normalized.into_owned(), handler));
        } else {
            self.static_routes[idx].insert(normalized.into_owned().into_boxed_str(), handler);
        }
    }

    pub fn add_websocket(&mut self, path: String, handler: Py<PyAny>) {
        let (normalized, _) = normalize_register(&path);
        self.websocket_routes
            .insert(normalized.into_owned(), handler);
    }

    pub fn build(self) -> FrozenRouter {
        let param_routes = std::array::from_fn(|idx| {
            let entries = &self.param_entries[idx];
            if entries.is_empty() {
                return None;
            }
            let mut router = matchit::Router::new();
            for (path, handler) in entries {
                if let Err(e) = router.insert(path, handler.clone()) {
                    tracing::warn!("Failed to insert parameterized route '{}': {}", path, e);
                }
            }
            Some(router)
        });

        FrozenRouter {
            static_routes: self.static_routes,
            param_routes,
            websocket_routes: self.websocket_routes,
        }
    }
}

impl Default for FrozenRouterBuilder {
    fn default() -> Self {
        Self::new()
    }
}

fn normalize_lookup(input: &str) -> Cow<'_, str> {
    let trimmed = input.trim();
    let needs_leading = !trimmed.starts_with('/');
    let needs_trailing_strip = trimmed.len() > 1 && trimmed.ends_with('/');

    if !needs_leading && !needs_trailing_strip && trimmed.len() == input.len() {
        return Cow::Borrowed(input);
    }
    if !needs_leading && !needs_trailing_strip {
        return Cow::Borrowed(trimmed);
    }

    let mut path = String::with_capacity(trimmed.len() + 1);
    if needs_leading {
        path.push('/');
    }
    path.push_str(trimmed);
    if path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    Cow::Owned(path)
}

fn normalize_register(input: &str) -> (Cow<'_, str>, bool) {
    let base = normalize_lookup(input);
    let bytes = base.as_bytes();
    let mut has_params = false;
    let mut in_param = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                if bytes.get(i + 1) == Some(&b'{') {
                    i += 2;
                    continue;
                }
                if in_param {
                    return (base, false);
                }
                in_param = true;
                has_params = true;
            }

            b'}' => {
                if bytes.get(i + 1) == Some(&b'}') {
                    i += 2;
                    continue;
                }
                if !in_param {
                    return (base, false);
                }
                in_param = false;
            }
            _ => {}
        }
        i += 1;
    }
    if in_param {
        return (base, false);
    }
    (base, has_params)
}
