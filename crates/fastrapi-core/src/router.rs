//! The route table that finds the handler for a request.
//! Static routes use a hash map, and routes with path parameters use a `matchit` router.
//! Each table is built one time and is not changed after that.

use crate::method::{HTTP_METHOD_COUNT, HttpMethod};
use ahash::AHashMap;
use matchit::Router;
use std::array::from_fn;
use std::borrow::Cow;
use std::mem::take;
use std::sync::Arc;
use strum::IntoEnumIterator;

#[derive(Clone, Copy, Debug)]
pub struct RoutePattern(pub &'static str);

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

    fn matches(self, value: &str) -> bool {
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
            Self::Uuid => {
                let mut groups = value.split('-');
                let shape: [Option<usize>; 6] = from_fn(|_| groups.next().map(str::len));
                matches!(
                    shape,
                    [Some(8), Some(4), Some(4), Some(4), Some(12), None]
                        | [Some(32), None, None, None, None, None]
                ) && value.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
            }
        }
    }
}

pub struct RouteTarget<H> {
    pub handler: Arc<H>,
    pub pattern: Box<str>,
    pub convertors: Box<[(String, PathConvertor)]>,
    pub host: Option<Box<str>>,
}

pub enum RouteMatch<'r, 'p, H> {
    Static(&'r RouteTarget<H>),
    Params(&'r RouteTarget<H>, matchit::Params<'r, 'p>),
}

#[expect(
    clippy::struct_field_names,
    reason = "Fields categorize routes by type: static_routes, param_routes"
)]
pub struct FrozenRouter<H> {
    static_routes: [AHashMap<Box<str>, RouteTarget<H>>; HTTP_METHOD_COUNT],
    param_routes: [Option<matchit::Router<RouteTarget<H>>>; HTTP_METHOD_COUNT],
    has_host_routes: bool,
}

impl<H> FrozenRouter<H> {
    #[inline]
    #[must_use]
    pub const fn has_host_routes(&self) -> bool {
        self.has_host_routes
    }

    #[inline(always)]
    pub fn resolve<'r, 'p>(
        &'r self,
        method: HttpMethod,
        path: &'p str,
        host: Option<&str>,
    ) -> Option<RouteMatch<'r, 'p, H>> {
        let idx = method as usize;
        let normalized = normalize_lookup(path);
        let host_allowed = |target: &RouteTarget<H>| {
            target
                .host
                .as_deref()
                .is_none_or(|pattern| host.is_some_and(|h| h == pattern))
        };
        if let Some(target) = self.static_routes[idx]
            .get(normalized)
            .filter(|target| host_allowed(target))
        {
            return Some(RouteMatch::Static(target));
        }
        let matched = self.param_routes[idx].as_ref()?.at(path).ok()?;
        let target = matched.value;
        // rejects the match, the way starlette's regex would
        if !convertors_match(&target.convertors, &matched.params) || !host_allowed(target) {
            return None;
        }
        Some(RouteMatch::Params(target, matched.params))
    }

    pub fn allowed_methods(&self, path: &str, host: Option<&str>) -> Vec<HttpMethod> {
        HttpMethod::iter()
            .filter(|method| self.resolve(*method, path, host).is_some())
            .collect()
    }
}

pub struct FrozenRouterBuilder<H> {
    static_routes: [AHashMap<Box<str>, RouteTarget<H>>; HTTP_METHOD_COUNT],
    param_entries: [Vec<(String, RouteTarget<H>)>; HTTP_METHOD_COUNT],
    has_host_routes: bool,
}

impl<H> FrozenRouterBuilder<H> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            static_routes: from_fn(|_| AHashMap::new()),
            param_entries: from_fn(|_| Vec::new()),
            has_host_routes: false,
        }
    }

    pub fn add_route(
        &mut self,
        method: HttpMethod,
        path: &str,
        handler: Arc<H>,
        host: Option<Box<str>>,
    ) {
        let idx = method as usize;
        let (normalized, has_params, convertors) = normalize_register(path);
        self.has_host_routes |= host.is_some();

        let target = RouteTarget {
            handler,
            pattern: normalized.as_ref().into(),
            convertors: convertors.into(),
            host,
        };

        if has_params {
            self.param_entries[idx].push((normalized.into_owned(), target));
        } else {
            self.static_routes[idx].insert(normalized.into_owned().into_boxed_str(), target);
        }
    }

    #[must_use]
    pub fn build(self) -> FrozenRouter<H> {
        let mut param_entries = self.param_entries;
        let param_routes = from_fn(|idx| {
            let entries = take(&mut param_entries[idx]);
            if entries.is_empty() {
                return None;
            }
            let mut router = Router::new();
            for (path, target) in entries {
                if let Err(e) = router.insert(&path, target) {
                    tracing::warn!("Failed to insert parameterized route '{path}': {e}");
                }
            }

            Some(router)
        });

        FrozenRouter {
            static_routes: self.static_routes,
            param_routes,
            has_host_routes: self.has_host_routes,
        }
    }
}

impl<H> Default for FrozenRouterBuilder<H> {
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

/// Checks each typed convertor against its captured value.
/// A parameter without a convertor always passes.
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
            let inner = &after[1..=close];
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
