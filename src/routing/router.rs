use crate::routing::types::{HTTP_METHOD_COUNT, HttpMethod, RouteHandler};
use ahash::AHashMap;
use pyo3::{Py, PyAny};
use std::{borrow::Cow, sync::Arc};

#[derive(Clone, Debug)]
pub struct RoutePattern(pub Arc<str>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathConvertor {
    Str,
    Int,
    Float,
    Uuid,
    Path,
}

impl PathConvertor {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "str" => Self::Str,
            "int" => Self::Int,
            "float" => Self::Float,
            "uuid" => Self::Uuid,
            "path" => Self::Path,
            _ => return None,
        })
    }

    fn matches(&self, value: &str) -> bool {
        match self {
            Self::Str | Self::Path => true,
            Self::Int => !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()),
            Self::Float => match value.split_once('.') {
                Some((int_part, frac_part)) => {
                    !int_part.is_empty()
                        && int_part.bytes().all(|b| b.is_ascii_digit())
                        && !frac_part.is_empty()
                        && frac_part.bytes().all(|b| b.is_ascii_digit())
                }
                None => false,
            },
            Self::Uuid => match value.split('-').map(str::len).collect::<Vec<_>>()[..] {
                [8, 4, 4, 4, 12] | [32] => {
                    value.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
                }
                _ => false,
            },
        }
    }
}

#[derive(Clone)]
pub struct RouteTarget {
    pub handler: Arc<RouteHandler>,
    pub pattern: Arc<str>,
    pub convertors: Arc<[(String, PathConvertor)]>,
}

pub enum RouteMatch<'a> {
    Static(RouteTarget),
    Params(RouteTarget, matchit::Params<'a, 'a>),
}

#[derive(Clone)]
pub struct FrozenRouter {
    static_routes: [AHashMap<Box<str>, RouteTarget>; HTTP_METHOD_COUNT],
    param_routes: [Option<matchit::Router<RouteTarget>>; HTTP_METHOD_COUNT],
    websocket_routes: AHashMap<String, Py<PyAny>>,
}

impl FrozenRouter {
    #[inline(always)]
    pub fn resolve<'a>(&'a self, method: HttpMethod, path: &'a str) -> Option<RouteMatch<'a>> {
        let idx = method as usize;
        let normalized = normalize_lookup(path);
        if let Some(target) = self.static_routes[idx].get(normalized) {
            return Some(RouteMatch::Static(target.clone()));
        }
        let matched = self.param_routes[idx].as_ref()?.at(path).ok()?;
        let target = matched.value.clone();
        // rejects the match, the way starlette's regex would
        if !convertors_match(&target.convertors, &matched.params) {
            return None;
        }
        Some(RouteMatch::Params(target, matched.params))
    }

    pub fn resolve_ws(&self, path: &str) -> Option<Py<PyAny>> {
        let normalized = normalize_lookup(path);
        self.websocket_routes.get(normalized).cloned()
    }
}

pub struct FrozenRouterBuilder {
    static_routes: [AHashMap<Box<str>, RouteTarget>; HTTP_METHOD_COUNT],
    param_entries: [Vec<(String, RouteTarget)>; HTTP_METHOD_COUNT],
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
        let (normalized, has_params, convertors) = normalize_register(&path);

        let target = RouteTarget {
            handler,
            pattern: Arc::from(normalized.as_ref()),
            convertors: convertors.into(),
        };

        if has_params {
            self.param_entries[idx].push((normalized.into_owned(), target));
        } else {
            self.static_routes[idx].insert(normalized.into_owned().into_boxed_str(), target);
        }
    }

    pub fn add_websocket(&mut self, path: String, handler: Py<PyAny>) {
        let (normalized, _, _) = normalize_register(&path);
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
            entries.iter().for_each(|(path, target)| {
                if let Err(e) = router.insert(path, target.clone()) {
                    tracing::warn!("Failed to insert parameterized route '{}': {}", path, e);
                }
            });

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

fn normalize_lookup(input: &str) -> &str {
    if input.len() > 1 {
        input.trim_end_matches('/')
    } else {
        input
    }
}

/// checks every typed convertor against its captured value; untyped params pass.
fn convertors_match(
    convertors: &[(String, PathConvertor)],
    params: &matchit::Params<'_, '_>,
) -> bool {
    convertors.iter().all(|(name, convertor)| {
        params
            .iter()
            .find(|(param_name, _)| *param_name == name)
            .is_none_or(|(_, value)| convertor.matches(value))
    })
}

fn normalize_register(input: &str) -> (Cow<'_, str>, bool, Vec<(String, PathConvertor)>) {
    let normalized = normalize_lookup(input);
    let base = if normalized.starts_with('/') {
        Cow::Borrowed(normalized)
    } else {
        let mut s = String::with_capacity(normalized.len() + 1);
        s.push('/');
        s.push_str(normalized);
        Cow::Owned(s)
    };

    let mut has_params = false;
    let mut in_param = false;
    let bytes = base.as_bytes();
    let mut i = 0;
    let len = bytes.len();

    while i < len {
        match bytes[i] {
            b'{' => {
                if i + 1 < len && bytes[i + 1] == b'{' {
                    i += 2;
                    continue;
                }
                if in_param {
                    return (base, false, Vec::new());
                }
                in_param = true;
                has_params = true;
            }
            b'}' => {
                if i + 1 < len && bytes[i + 1] == b'}' {
                    i += 2;
                    continue;
                }
                if !in_param {
                    return (base, false, Vec::new());
                }
                in_param = false;
            }
            _ => {}
        }
        i += 1;
    }

    if in_param {
        return (base, false, Vec::new());
    }

    if !base.contains(':') {
        return (base, has_params, Vec::new());
    }

    let mut out = String::with_capacity(base.len());
    let mut convertors: Vec<(String, PathConvertor)> = Vec::new();
    let mut rest = base.as_ref();
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        if let Some(close) = after[1..].find('}') {
            let inner = &after[1..1 + close];
            let (name, convertor) = match inner.split_once(':') {
                Some((name, conv)) => (name, PathConvertor::from_name(conv)),
                None => (inner, Some(PathConvertor::Str)),
            };
            match convertor {
                Some(PathConvertor::Path) => {
                    has_params = true;
                    out.push('{');
                    out.push('*');
                    out.push_str(name);
                    out.push('}');
                    convertors.push((name.to_string(), PathConvertor::Path));
                }
                Some(convertor) => {
                    has_params = true;
                    out.push('{');
                    out.push_str(name);
                    out.push('}');
                    convertors.push((name.to_string(), convertor));
                }
                None => out.push_str(&after[..close + 2]),
            }
            rest = &after[close + 2..];
        } else {
            out.push_str(after);
            rest = "";
        }
    }
    out.push_str(rest);

    (
        if out == base { base } else { Cow::Owned(out) },
        has_params,
        convertors,
    )
}
