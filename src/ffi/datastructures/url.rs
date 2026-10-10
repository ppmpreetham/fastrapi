//! Starlette-compatible `URL`.
//!
//! Port of `starlette.datastructures.URL`: string-backed, components parsed
//! lazily on first access via `CPython`'s `urlsplit` (ground-truth semantics for
//! IPv6/userinfo edge cases), cached for the object's lifetime. Construction
//! from an ASGI scope and query surgery run natively.

use std::sync::RwLock;

use pyo3::exceptions::{PyAssertionError, PyTypeError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use pyo3::class::basic::CompareOp;
use pyo3::types::PyTuple;
use std::collections::hash_map::DefaultHasher;

struct SplitUrl {
    scheme: Option<String>,
    netloc: Option<String>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
    hostname: Option<String>,
    port: Option<u16>,
    username: Option<String>,
    password: Option<String>,
}

impl Clone for SplitUrl {
    fn clone(&self) -> Self {
        Self {
            scheme: self.scheme.clone(),
            netloc: self.netloc.clone(),
            path: self.path.clone(),
            query: self.query.clone(),
            fragment: self.fragment.clone(),
            hostname: self.hostname.clone(),
            port: self.port,
            username: self.username.clone(),
            password: self.password.clone(),
        }
    }
}

impl SplitUrl {
    fn from_netloc(
        parts: (
            Option<String>,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
        ),
    ) -> Self {
        let (scheme, netloc, path, query, fragment) = parts;
        let (hostname, port, username, password) = match &netloc {
            Some(netloc) => parse_netloc(netloc),
            None => (None, None, None, None),
        };
        Self {
            scheme,
            netloc,
            path,
            query,
            fragment,
            hostname,
            port,
            username,
            password,
        }
    }

    /// `SplitResult.geturl()`: empty components are omitted.
    fn geturl(&self) -> String {
        let mut out = String::with_capacity(64);
        if let Some(scheme) = &self.scheme {
            out.push_str(scheme);
            out.push_str("://");
        }
        if let Some(netloc) = &self.netloc {
            out.push_str(netloc);
        }
        out.push_str(&self.path);
        if let Some(query) = &self.query
            && !query.is_empty()
        {
            out.push('?');
            out.push_str(query);
        }
        if let Some(fragment) = &self.fragment
            && !fragment.is_empty()
        {
            out.push('#');
            out.push_str(fragment);
        }
        out
    }
}

fn split_cached(url: &str) -> PyResult<SplitUrl> {
    Python::attach(|py| {
        let parts = py
            .import("urllib.parse")?
            .getattr("urlsplit")?
            .call1((url,))?;
        let get = |name: &str| -> PyResult<Option<String>> {
            let text: String = parts.getattr(name)?.extract()?;
            Ok((!text.is_empty()).then_some(text))
        };
        let scheme = get("scheme")?;
        let netloc = get("netloc")?;
        let path: String = parts.getattr("path")?.extract()?;
        let query = get("query")?;
        let fragment = get("fragment")?;
        Ok(SplitUrl::from_netloc((
            scheme, netloc, path, query, fragment,
        )))
    })
}

/// Extract `(hostname, port, username, password)` from a netloc, matching
/// `urlsplit` semantics (bracketed IPv6 hosts keep their brackets in `host`,
/// `hostname` drops them and lowercases).
fn parse_netloc(netloc: &str) -> (Option<String>, Option<u16>, Option<String>, Option<String>) {
    let (userinfo, hostport) = match netloc.rsplit_once('@') {
        Some((userinfo, hostport)) => (Some(userinfo), hostport),
        None => (None, netloc),
    };
    let (username, password) = match userinfo {
        Some(userinfo) => match userinfo.split_once(':') {
            Some((user, pass)) => (
                (!user.is_empty()).then(|| user.to_string()),
                (!pass.is_empty()).then(|| pass.to_string()),
            ),
            None => ((!userinfo.is_empty()).then(|| userinfo.to_string()), None),
        },
        None => (None, None),
    };
    let (host, port) = if hostport.starts_with('[') {
        match hostport.find(']') {
            Some(end) => {
                let port = hostport[end + 1..]
                    .strip_prefix(':')
                    .and_then(|p| p.parse().ok());
                (Some(&hostport[..=end]), port)
            }
            None => (Some(hostport), None),
        }
    } else {
        match hostport.rsplit_once(':') {
            Some((host, port)) => (Some(host), port.parse().ok()),
            None => (Some(hostport), None),
        }
    };
    let hostname = host.map(|h| {
        h.trim_matches(|c| c == '[' || c == ']')
            .to_ascii_lowercase()
    });
    (hostname, port, username, password)
}

/// Port of `starlette._utils.parse_host_header` (character-set validation in
/// place of the module's regex). Returns `(authority, port_is_valid)`.
fn parse_host_header(header: &str) -> Option<(String, bool)> {
    let (host, port) = if let Some(rest) = header.strip_prefix('[') {
        let end = rest.find(']')?;
        let inner = &rest[..end];
        let after = &rest[end + 1..];
        let port = match after.strip_prefix(':') {
            Some(port) => Some(port),
            None if after.is_empty() => None,
            None => return None,
        };
        (inner, port)
    } else {
        match header.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (header, None),
        }
    };
    if host.is_empty() || !host.bytes().all(is_host_byte) {
        return None;
    }
    let valid_port = match port {
        None => true,
        Some(port) if !port.bytes().all(|b| b.is_ascii_digit()) => false,
        Some(port) => {
            port.trim_start_matches('0').len() <= 5
                && port.parse::<u32>().unwrap_or(u32::MAX) <= 65535
        }
    };
    Some((header.to_string(), valid_port))
}

fn is_host_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'.' | b'_'
                | b'~'
                | b'%'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b'-'
        )
}

/// Port of starlette's scope-based URL construction.
fn url_from_scope(scope: &Bound<'_, PyAny>) -> PyResult<String> {
    let get_scope_str = |name: &str| -> PyResult<Option<String>> {
        let value = scope.get_item(name)?;
        if value.is_none() {
            Ok(None)
        } else {
            Ok(Some(value.extract()?))
        }
    };
    let scheme: String = get_scope_str("scheme")?.unwrap_or_else(|| "http".to_string());
    let path: String = get_scope_str("path")?.unwrap_or_else(|| "/".to_string());
    let query_string: Vec<u8> = match scope.get_item("query_string")? {
        v if !v.is_none() => v.extract().unwrap_or_default(),
        _ => Vec::new(),
    };

    let mut host_header: Option<String> = None;
    if let Ok(headers) = scope.get_item("headers") {
        for pair in headers.try_iter()? {
            let pair = pair?;
            let key: Vec<u8> = pair.get_item(0)?.extract()?;
            if key.as_slice() == b"host" {
                let value: Vec<u8> = pair.get_item(1)?.extract()?;
                host_header = Some(String::from_utf8_lossy(&value).into_owned());
                break;
            }
        }
    }

    let netloc = if let Some((authority, true)) = host_header.as_deref().and_then(parse_host_header) { Some(authority) } else {
        let server: Option<(String, u16)> = match scope.get_item("server")? {
            v if !v.is_none() => v.extract().ok(),
            _ => None,
        };
        server.map(|(host, port)| {
            let default_port = match scheme.as_str() {
                "https" => 443,
                "ws" => 80,
                "wss" => 443,
                _ => 80,
            };
            let host = if host.contains(':') && !host.starts_with('[') {
                format!("[{host}]")
            } else {
                host
            };
            if port == default_port {
                host
            } else {
                format!("{host}:{port}")
            }
        })
    };

    let query = String::from_utf8_lossy(&query_string).into_owned();
    Ok(match netloc {
        Some(netloc) => SplitUrl::from_netloc((
            Some(scheme),
            Some(netloc),
            path,
            (!query.is_empty()).then_some(query),
            None,
        ))
        .geturl(),
        None => {
            if query.is_empty() {
                path
            } else {
                format!("{path}?{query}")
            }
        }
    })
}

#[pyclass(name = "URL", module = "fastrapi.datastructures", frozen)]
pub struct PyURL {
    /// Full URL string; the single source of truth.
    url: String,
    /// Lazily-populated parse cache.
    parts: RwLock<Option<SplitUrl>>,
}

impl PyURL {
    pub(crate) fn from_string(url: String) -> Self {
        Self {
            url,
            parts: RwLock::new(None),
        }
    }

    /// Build from an ASGI scope, as starlette's `URL(scope=...)` does.
    pub(crate) fn from_scope(scope: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self::from_string(url_from_scope(scope)?))
    }

    fn components(&self) -> PyResult<SplitUrl> {
        if let Some(parts) = self.parts.read().unwrap().as_ref() {
            return Ok(parts.clone());
        }
        let parsed = split_cached(&self.url)?;
        *self.parts.write().unwrap() = Some(parsed.clone());
        Ok(parsed)
    }
}

fn invalid_component(key: &str) -> PyErr {
    PyTypeError::new_err(format!("Invalid URL component: '{key}'"))
}

/// `urllib.parse.urlencode` quotes with `quote_plus`: spaces become `+`.
fn encode_pair((key, value): &(String, String)) -> String {
    let key: String = form_urlencoded::byte_serialize(key.as_bytes()).collect();
    let value: String = form_urlencoded::byte_serialize(value.as_bytes()).collect();
    format!("{key}={value}").replace("%20", "+")
}

#[pymethods]
impl PyURL {
    #[new]
    #[pyo3(signature = (url = "", ** components))]
    fn new(_py: Python<'_>, url: &str, components: Option<&Bound<'_, PyDict>>) -> PyResult<Self> {
        let Some(components) = components else {
            return Ok(Self::from_string(url.to_string()));
        };
        if let Some(scope) = components.get_item("scope")? {
            if !url.is_empty() {
                return Err(PyAssertionError::new_err(
                    "Cannot set both \"url\" and \"scope\".",
                ));
            }
            return Self::from_scope(&scope);
        }
        let base = Self::from_string(url.to_string());
        let mut parts = base.components()?;
        for (key, value) in components.iter() {
            let key: String = key.extract()?;
            let text: String = value.str()?.to_string();
            match key.as_str() {
                "scheme" => parts.scheme = (!text.is_empty()).then_some(text),
                "netloc" => parts.netloc = (!text.is_empty()).then_some(text),
                "path" => parts.path = text,
                "query" => parts.query = (!text.is_empty()).then_some(text),
                "fragment" => parts.fragment = (!text.is_empty()).then_some(text),
                _ => return Err(invalid_component(&key)),
            }
        }
        Ok(Self::from_string(parts.geturl()))
    }

    #[getter]
    fn py_components(slf: &Bound<'_, Self>) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let parts = slf.borrow().components()?;
        let namedtuple = py.import("collections")?.getattr("namedtuple")?.call1((
            "Components",
            "scheme netloc path query fragment hostname port username password",
        ))?;
        Ok(namedtuple
            .call1((
                parts.scheme.unwrap_or_default(),
                parts.netloc.unwrap_or_default(),
                parts.path,
                parts.query.unwrap_or_default(),
                parts.fragment.unwrap_or_default(),
                parts.hostname,
                parts.port,
                parts.username,
                parts.password,
            ))?
            .unbind())
    }

    #[getter]
    fn scheme(&self) -> PyResult<Option<String>> {
        Ok(self.components()?.scheme)
    }

    #[getter]
    fn netloc(&self) -> PyResult<Option<String>> {
        Ok(self.components()?.netloc)
    }

    #[getter]
    fn hostname(&self) -> PyResult<Option<String>> {
        Ok(self.components()?.hostname)
    }

    #[getter]
    fn port(&self) -> PyResult<Option<u16>> {
        Ok(self.components()?.port)
    }

    #[getter]
    fn username(&self) -> PyResult<Option<String>> {
        Ok(self.components()?.username)
    }

    #[getter]
    fn password(&self) -> PyResult<Option<String>> {
        Ok(self.components()?.password)
    }

    #[getter]
    fn path(&self) -> PyResult<String> {
        Ok(self.components()?.path)
    }

    #[getter]
    fn query(&self) -> PyResult<Option<String>> {
        Ok(self.components()?.query)
    }

    #[getter]
    fn fragment(&self) -> PyResult<Option<String>> {
        Ok(self.components()?.fragment)
    }

    #[getter]
    fn is_secure(&self) -> PyResult<bool> {
        Ok(matches!(
            self.components()?.scheme.as_deref(),
            Some("https" | "wss")
        ))
    }

    #[getter]
    fn is_absolute_url(&self) -> PyResult<bool> {
        let parts = self.components()?;
        Ok(parts.scheme.is_some() && parts.netloc.is_some())
    }

    #[getter]
    fn original_url(&self) -> String {
        self.url.clone()
    }

    fn __str__(&self) -> String {
        self.url.clone()
    }

    fn __repr__(&self) -> PyResult<String> {
        let url = match &self.components()?.password {
            Some(_) => "********".to_string(),
            None => self.url.clone(),
        };
        Ok(format!("URL('{url}')"))
    }

    pub fn __hash__(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        self.url.hash(&mut hasher);
        hasher.finish()
    }

    fn __richcmp__(
        &self,
        other: &Bound<'_, PyAny>,
        op: CompareOp,
    ) -> PyResult<bool> {
        let other_str = match other.cast::<PyURL>() {
            Ok(other) => other.borrow().url.clone(),
            Err(_) => other.str()?.to_string(),
        };
        Ok(match op {
            CompareOp::Eq => self.url == other_str,
            CompareOp::Ne => self.url != other_str,
            _ => false,
        })
    }

    #[pyo3(signature = (**kwargs))]
    fn replace<'a>(
        slf: &Bound<'a, Self>,
        kwargs: Option<&Bound<'a, PyDict>>,
    ) -> PyResult<Bound<'a, Self>> {
        let mut parts = slf.borrow().components()?;
        let mut kwargs = kwargs;
        let netloc_dict: Option<Bound<'a, PyDict>>;
        if let Some(dict) = kwargs
            && (dict.get_item("username")?.is_some()
                || dict.get_item("password")?.is_some()
                || dict.get_item("hostname")?.is_some()
                || dict.get_item("port")?.is_some())
        {
            let get = |name: &str| -> PyResult<Option<String>> {
                match dict.get_item(name)? {
                    Some(v) if !v.is_none() => Ok(Some(v.extract::<String>()?)),
                    _ => Ok(None),
                }
            };
            let hostname = get("hostname")?;
            let port = dict
                .get_item("port")?
                .filter(|v| !v.is_none())
                .map(|v| v.extract::<u16>())
                .transpose()?
                .or(parts.port);
            let username = match get("username")? {
                Some(username) => Some(username),
                None => parts.username.clone(),
            };
            let password = match get("password")? {
                Some(password) => Some(password),
                None => parts.password.clone(),
            };
            let hostname = match hostname {
                Some(hostname) => hostname,
                None => parts
                    .netloc
                    .as_deref()
                    .and_then(|netloc| netloc.rsplit_once('@').map(|(_, h)| h))
                    .unwrap_or_default()
                    .rsplit_once(':')
                    .filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit()))
                    .map_or_else(
                        || {
                            parts
                                .netloc
                                .as_deref()
                                .and_then(|netloc| netloc.rsplit_once('@').map(|(_, h)| h))
                                .unwrap_or_default()
                                .to_string()
                        },
                        |(host, _)| host.to_string(),
                    ),
            };
            let mut netloc = hostname;
            if let Some(port) = port {
                netloc.push_str(&format!(":{port}"));
            }
            if let Some(username) = username {
                match password {
                    Some(password) => netloc = format!("{username}:{password}@{netloc}"),
                    None => netloc = format!("{username}@{netloc}"),
                }
            }
            let merged = PyDict::new(slf.py());
            for (key, value) in dict.iter() {
                let key: String = key.extract()?;
                if matches!(key.as_str(), "username" | "password" | "hostname" | "port") {
                    continue;
                }
                merged.set_item(key, value)?;
            }
            merged.set_item("netloc", netloc)?;
            netloc_dict = Some(merged);
            kwargs = netloc_dict.as_ref();
        }
        if let Some(dict) = kwargs {
            for (key, value) in dict.iter() {
                let key: String = key.extract()?;
                let text: String = value.str()?.to_string();
                match key.as_str() {
                    "scheme" => parts.scheme = (!text.is_empty()).then_some(text),
                    "netloc" => parts.netloc = (!text.is_empty()).then_some(text),
                    "path" => parts.path = text,
                    "query" => parts.query = (!text.is_empty()).then_some(text),
                    "fragment" => parts.fragment = (!text.is_empty()).then_some(text),
                    _ => return Err(invalid_component(&key)),
                }
            }
        }
        Py::new(slf.py(), Self::from_string(parts.geturl())).map(|obj| obj.into_bound(slf.py()))
    }

    #[pyo3(signature = (**kwargs))]
    fn include_query_params<'a>(
        slf: &Bound<'a, Self>,
        kwargs: Option<&Bound<'a, PyDict>>,
    ) -> PyResult<Bound<'a, Self>> {
        let py = slf.py();
        let existing = slf.borrow().components()?.query.unwrap_or_default();
        let mut pairs: Vec<(String, String)> = parse_qsl_pairs(py, &existing)?;
        if let Some(kwargs) = kwargs {
            let updates = urlencode_pairs(kwargs)?;
            let keys: Vec<&str> = updates.iter().map(|(k, _)| k.as_str()).collect();
            pairs.retain(|(key, _)| !keys.contains(&key.as_str()));
            pairs.extend(updates);
        }
        let query = pairs.iter().map(encode_pair).collect::<Vec<_>>().join("&");
        Self::replace(slf, Some(&replace_kwargs(py, &query)?))
    }

    #[pyo3(signature = (**kwargs))]
    fn replace_query_params<'a>(
        slf: &Bound<'a, Self>,
        kwargs: Option<&Bound<'a, PyDict>>,
    ) -> PyResult<Bound<'a, Self>> {
        let py = slf.py();
        let pairs = match kwargs {
            Some(kwargs) => urlencode_pairs(kwargs)?,
            None => Vec::new(),
        };
        let query = pairs.iter().map(encode_pair).collect::<Vec<_>>().join("&");
        Self::replace(slf, Some(&replace_kwargs(py, &query)?))
    }

    #[pyo3(signature = (*keys))]
    fn remove_query_params<'a>(
        slf: &Bound<'a, Self>,
        keys: &Bound<'a, PyTuple>,
    ) -> PyResult<Bound<'a, Self>> {
        let py = slf.py();
        let existing = slf.borrow().components()?.query.unwrap_or_default();
        let remove: Vec<String> = keys.iter().map(|k| k.extract()).collect::<PyResult<_>>()?;
        let pairs: Vec<(String, String)> = parse_qsl_pairs(py, &existing)?
            .into_iter()
            .filter(|(key, _)| !remove.contains(key))
            .collect();
        let query = pairs.iter().map(encode_pair).collect::<Vec<_>>().join("&");
        Self::replace(slf, Some(&replace_kwargs(py, &query)?))
    }

    fn make_absolute_url<'a>(slf: &Bound<'a, Self>, base_url: &str) -> PyResult<Bound<'a, Self>> {
        let this = slf.borrow();
        let parts = this.components()?;
        if parts.netloc.is_some() {
            drop(this);
            return Ok(slf.clone());
        }
        let base = PyURL::from_string(base_url.to_string());
        let b = base.components()?;
        let absolute = format!(
            "{}://{}{}",
            b.scheme.as_deref().unwrap_or("http"),
            b.netloc.as_deref().unwrap_or_default(),
            parts.path,
        );
        drop(this);
        Py::new(slf.py(), PyURL::from_string(absolute)).map(|obj| obj.into_bound(slf.py()))
    }

    fn __truediv__(&self, other: &str) -> String {
        format!(
            "{}/{}",
            self.url.trim_end_matches('/'),
            other.trim_start_matches('/')
        )
    }
}

/// `urllib.parse.parse_qsl` with `keep_blank_values=True`.
fn parse_qsl_pairs(_py: Python<'_>, query: &str) -> PyResult<Vec<(String, String)>> {
    Ok(form_urlencoded::parse(query.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect())
}

fn urlencode_pairs(kwargs: &Bound<'_, PyDict>) -> PyResult<Vec<(String, String)>> {
    let mut pairs = Vec::with_capacity(kwargs.len());
    for (key, value) in kwargs.iter() {
        let key: String = key.extract()?;
        if value.is_none() {
            continue;
        }
        let text: String = value.str()?.to_string();
        pairs.push((key, text));
    }
    Ok(pairs)
}

fn replace_kwargs<'py>(py: Python<'py>, query: &str) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("query", query)?;
    Ok(dict)
}
