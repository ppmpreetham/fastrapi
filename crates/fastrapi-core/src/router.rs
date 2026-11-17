//! The route table that finds the handler for a request.
//! Static routes use a hash map, and routes with path parameters use a `matchit` router.
//! Each table is built one time and is not changed after that.

use crate::method::{HTTP_METHOD_COUNT, HttpMethod};
use ahash::AHashMap;
use matchit::Router;
use std::array::from_fn;
use std::borrow::Cow;
use std::collections::hash_map::Entry;
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

fn all_digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
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
            Self::Int => all_digits(value),
            Self::Float => value.split_once('.').map_or_else(
                || all_digits(value),
                |(int, frac)| all_digits(int) && all_digits(frac),
            ),
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
    /// The methods of the route this target belongs to, as [`HttpMethod::bit`] flags.
    /// They make the `Allow` header of a 405, like Starlette's `Route.handle`.
    pub allow: u8,
    /// The position of the route in registration order, as Starlette tries routes in order.
    order: usize,
    /// A later route registered for the same method and path, tried when this one's host or
    /// convertors do not match, as Starlette tries routes in registration order.
    next: Option<Box<Self>>,
}

impl<H> std::fmt::Debug for RouteTarget<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteTarget")
            .field("pattern", &self.pattern)
            .field("convertors", &self.convertors)
            .field("host", &self.host)
            .field("allow", &self.allow)
            .field("next", &self.next)
            .finish_non_exhaustive()
    }
}

impl<H> RouteTarget<H> {
    /// Adds `target` after the last route in this chain, so the first registered route wins.
    fn push(&mut self, target: Self) {
        let mut slot = &mut self.next;
        while let Some(next) = slot {
            slot = &mut next.next;
        }
        *slot = Some(Box::new(target));
    }

    fn chain(&self) -> impl Iterator<Item = &Self> {
        std::iter::successors(Some(self), |target| target.next.as_deref())
    }
}

pub enum RouteMatch<'r, 'p, H> {
    Static(&'r RouteTarget<H>),
    Params(&'r RouteTarget<H>, matchit::Params<'r, 'p>),
}

impl<'r, H> RouteMatch<'r, '_, H> {
    #[must_use]
    pub const fn target(&self) -> &'r RouteTarget<H> {
        match self {
            Self::Static(target) | Self::Params(target, _) => target,
        }
    }
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

impl<H> std::fmt::Debug for FrozenRouter<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let static_routes: usize = self.static_routes.iter().map(|routes| routes.len()).sum();
        f.debug_struct("FrozenRouter")
            .field("static_routes", &static_routes)
            .field("has_host_routes", &self.has_host_routes)
            .finish_non_exhaustive()
    }
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
        let host_allowed = |target: &RouteTarget<H>| {
            target
                .host
                .as_deref()
                .is_none_or(|pattern| host.is_some_and(|h| h == pattern))
        };
        if let Some(target) = self.static_routes[idx].get(path) {
            if host_allowed(target) {
                return Some(RouteMatch::Static(target));
            }
            if let Some(target) = target.chain().skip(1).find(|target| host_allowed(target)) {
                return Some(RouteMatch::Static(target));
            }
        }
        let matched = self.param_routes[idx].as_ref()?.at(path).ok()?;
        let target = matched.value.chain().find(|target| {
            convertors_match(&target.convertors, &matched.params) && host_allowed(target)
        })?;
        Some(RouteMatch::Params(target, matched.params))
    }

    /// The methods of the first registered route that matches `path`, or 0 when none does.
    /// Like Starlette's partial match, only that route's methods go in the `Allow` header.
    pub fn allowed_methods(&self, path: &str, host: Option<&str>) -> u8 {
        HttpMethod::iter()
            .filter_map(|method| self.resolve(method, path, host))
            .map(|route_match| route_match.target())
            .min_by_key(|target| target.order)
            .map_or(0, |target| target.allow)
    }
}

pub struct FrozenRouterBuilder<H> {
    static_routes: [AHashMap<Box<str>, RouteTarget<H>>; HTTP_METHOD_COUNT],
    param_entries: [Vec<(String, RouteTarget<H>)>; HTTP_METHOD_COUNT],
    has_host_routes: bool,
    route_count: usize,
}

impl<H> FrozenRouterBuilder<H> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            static_routes: from_fn(|_| AHashMap::new()),
            param_entries: from_fn(|_| Vec::new()),
            has_host_routes: false,
            route_count: 0,
        }
    }

    /// Adds a route for `method`. `allow` holds the bits of every method of the
    /// route, so a 405 on its path lists them.
    pub fn add_route(
        &mut self,
        method: HttpMethod,
        path: &str,
        handler: Arc<H>,
        host: Option<Box<str>>,
        allow: u8,
    ) {
        let idx = method as usize;
        let (normalized, has_params, convertors) = normalize_register(path);
        self.has_host_routes |= host.is_some();

        let target = RouteTarget {
            handler,
            pattern: normalized.as_ref().into(),
            convertors: convertors.into(),
            host,
            allow,
            order: self.route_count,
            next: None,
        };
        self.route_count += 1;

        if has_params {
            self.param_entries[idx].push((normalized.into_owned(), target));
        } else {
            match self.static_routes[idx].entry(normalized.into_owned().into_boxed_str()) {
                Entry::Occupied(mut first) => first.get_mut().push(target),
                Entry::Vacant(slot) => {
                    slot.insert(target);
                }
            }
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
            // Routes with the same pattern share one matchit entry, in registration order.
            let mut grouped: Vec<(String, RouteTarget<H>)> = Vec::with_capacity(entries.len());
            for (path, target) in entries {
                match grouped.iter_mut().find(|(existing, _)| *existing == path) {
                    Some((_, first)) => first.push(target),
                    None => grouped.push((path, target)),
                }
            }
            let mut router = Router::new();
            for (path, target) in grouped {
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
    let base = if input.starts_with('/') {
        Cow::Borrowed(input)
    } else {
        let mut s = String::with_capacity(input.len() + 1);
        s.push('/');
        s.push_str(input);
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
