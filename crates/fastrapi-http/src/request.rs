//! This module contains the `HTTPConnection` and `Request` Python classes, which are compatible with starlette.
//! They give Python access to the URL, headers, cookies, parameters, and body of a request.

use crate::exceptions::http_exception;
use crate::form;
use crate::middleware::SessionHandle;
use crate::request_input::{AsgiMount, RequestInput};
use crate::url::PyURL;
use axum::http::HeaderMap;
use axum::http::header::COOKIE;
use bytes::Bytes;
use cookie::Cookie;
use fastrapi_pyutil::imports::{SIMPLE_COOKIE, STARLETTE_HEADERS, STARLETTE_STATE, TYPES};
use fastrapi_runtime::py_bridge;
use fastrapi_runtime::server_state::current_server;
use futures_util::FutureExt;
use percent_encoding::percent_decode_str;
use pyo3::exceptions::{PyAssertionError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict, PyList, PyString, PyTuple};
use pyo3::{Py, PyClassInitializer, intern};
use std::borrow::Cow;
use std::net::SocketAddr;
use std::str::from_utf8;
use std::sync::{Arc, OnceLock};
use tokio::sync::OnceCell;

fastrapi_pyutil::cached_py_import!(
    STARLETTE_QUERY_PARAMS,
    "starlette.datastructures",
    "QueryParams"
);
fastrapi_pyutil::cached_py_import!(JSON_MODULE, "json");
fastrapi_pyutil::cached_py_import!(ADDRESS, "starlette.datastructures", "Address");

#[pyclass(
    frozen,
    new = "from_fields",
    name = "ClientInfo",
    module = "fastrapi.request",
    get_all,
    from_py_object,
    eq
)]
#[derive(Clone, PartialEq, Eq)]
pub struct PyClientInfo {
    pub host: String,
    pub port: u16,
}

#[pyclass(name = "AppInfo", module = "fastrapi.request", skip_from_py_object)]
#[derive(Clone)]
pub struct PyAppInfo {
    pub scope: Py<PyAny>,
}

#[pymethods]
impl PyAppInfo {
    #[new]
    const fn new(scope: Py<PyAny>) -> Self {
        Self { scope }
    }

    #[getter]
    fn state(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        if !scope.contains("state")? {
            let namespace = TYPES
                .get(py)?
                .call_method0(intern!(py, "SimpleNamespace"))?;
            scope.set_item(intern!(py, "state"), namespace)?;
        }
        Ok(scope.get_item(intern!(py, "state"))?.into())
    }
}

/// A base class for incoming HTTP connections, that is used to provide
/// any functionality that is common to both `Request` and `WebSocket`.
#[pyclass(
    subclass,
    name = "HTTPConnection",
    module = "fastrapi.request",
    skip_from_py_object
)]
#[derive(Clone)]
pub struct PyHTTPConnection {
    pub scope: Py<PyAny>,
    #[pyo3(get, set)]
    pub receive: Py<PyAny>,
    headers_cache: OnceLock<Py<PyAny>>,
    query_params_cache: OnceLock<Py<PyAny>>,
    cookies_cache: OnceLock<Py<PyAny>>,
    scope_complete: OnceLock<()>,
    server: Option<SocketAddr>,
}

impl PyHTTPConnection {
    #[must_use]
    pub const fn from_scope(scope: Py<PyAny>, receive: Py<PyAny>) -> Self {
        Self {
            scope,
            receive,
            headers_cache: OnceLock::new(),
            query_params_cache: OnceLock::new(),
            cookies_cache: OnceLock::new(),
            scope_complete: OnceLock::new(),
            server: None,
        }
    }

    #[must_use]
    pub const fn with_server(mut self, server: Option<SocketAddr>) -> Self {
        self.server = server;
        self
    }
}

#[must_use]
pub fn decode_path(path: &str) -> Cow<'_, str> {
    percent_decode_str(path).decode_utf8_lossy()
}

fn host_server(scope: &Bound<'_, PyDict>) -> PyResult<Option<(String, u16)>> {
    let py = scope.py();
    let Some(headers) = scope.get_item(intern!(py, "headers"))? else {
        return Ok(None);
    };
    for pair in headers.try_iter()? {
        let (name, value): (Vec<u8>, Vec<u8>) = pair?.extract()?;
        if name.eq_ignore_ascii_case(b"host") {
            let host = String::from_utf8_lossy(&value);
            return Ok(Some(match host.rsplit_once(':') {
                Some((name, port)) if !name.ends_with(']') || host.starts_with('[') => {
                    match port.parse() {
                        Ok(port) => (name.to_owned(), port),
                        Err(_) => (host.into_owned(), 80),
                    }
                }
                _ => (host.into_owned(), 80),
            }));
        }
    }
    Ok(None)
}

fn complete_scope(scope: &Bound<'_, PyAny>, server: Option<SocketAddr>) -> PyResult<()> {
    let Ok(scope) = scope.cast::<PyDict>() else {
        return Ok(());
    };
    let py = scope.py();
    if let Some(headers) = scope.get_item(intern!(py, "headers"))?
        && let Ok(headers) = headers.cast::<PyScopeHeaders>()
    {
        scope.set_item(intern!(py, "headers"), headers.get().to_list(py)?)?;
    }
    if let Some(query) = scope.get_item(intern!(py, "query_string"))?
        && let Ok(text) = query.cast::<PyString>()
    {
        scope.set_item(
            intern!(py, "query_string"),
            PyBytes::new(py, text.to_str()?.as_bytes()),
        )?;
    }
    if !scope.contains(intern!(py, "raw_path"))?
        && let Some(path) = scope.get_item(intern!(py, "path"))?
    {
        let path = path.cast_into::<PyString>()?;
        scope.set_item(
            intern!(py, "raw_path"),
            PyBytes::new(py, path.to_str()?.as_bytes()),
        )?;
    }
    if !scope.contains(intern!(py, "root_path"))? {
        let root_path = match scope.get_item(intern!(py, "app"))? {
            Some(app) => app
                .getattr(intern!(py, "root_path"))
                .unwrap_or_else(|_| intern!(py, "").clone().into_any()),
            None => intern!(py, "").clone().into_any(),
        };
        scope.set_item(intern!(py, "root_path"), root_path)?;
    }
    if !scope.contains(intern!(py, "scheme"))? {
        scope.set_item(intern!(py, "scheme"), intern!(py, "http"))?;
    }
    if !scope.contains(intern!(py, "http_version"))? {
        scope.set_item(intern!(py, "http_version"), intern!(py, "1.1"))?;
    }
    if !scope.contains(intern!(py, "server"))? {
        match server {
            Some(server) => scope.set_item(
                intern!(py, "server"),
                (server.ip().to_string(), server.port()),
            )?,
            None => scope.set_item(intern!(py, "server"), host_server(scope)?)?,
        }
    }
    Ok(())
}

#[pymethods]
impl PyHTTPConnection {
    #[new]
    #[pyo3(signature = (scope, receive=None))]
    pub fn new(py: Python<'_>, scope: Py<PyAny>, receive: Option<Py<PyAny>>) -> PyResult<Self> {
        let scope_bound = scope.bind(py);

        if let Ok(scope_type) = scope_bound.get_item(intern!(py, "type")) {
            let type_str: String = scope_type.extract()?;
            if type_str != "http" && type_str != "websocket" {
                return Err(PyValueError::new_err(
                    "Scope type must be 'http' or 'websocket'",
                ));
            }
        }

        Ok(Self::from_scope(
            scope,
            receive.unwrap_or_else(|| py.None()),
        ))
    }

    #[getter(scope)]
    fn get_scope<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let scope = self.scope.bind(py);
        if self.scope_complete.get().is_none() {
            complete_scope(scope, self.server)?;
            _ = self.scope_complete.set(());
        }
        Ok(scope.clone())
    }

    #[setter(scope)]
    fn set_scope(&mut self, scope: Py<PyAny>) {
        self.scope = scope;
        self.scope_complete = OnceLock::new();
    }

    #[getter]
    pub fn app(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        if let Ok(app) = scope.get_item(intern!(py, "app"))
            && !app.is_none()
        {
            return Ok(app.into());
        }
        Ok(PyAppInfo::new(self.scope.clone_ref(py))
            .into_pyobject(py)?
            .into_any()
            .unbind())
    }

    #[getter]
    pub fn url(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let url = PyURL::from_scope(self.scope.bind(py))?;
        Py::new(py, url).map(Py::into_any)
    }

    #[getter]
    pub fn base_url(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);

        if let Ok(base_scope) = scope.call_method0(intern!(py, "copy")) {
            _ = base_scope.set_item(intern!(py, "path"), "/");
            _ = base_scope.set_item(intern!(py, "query_string"), PyBytes::new(py, b""));
            let root_path = scope
                .get_item(intern!(py, "app_root_path"))
                .or_else(|_| scope.get_item(intern!(py, "root_path")))
                .unwrap_or_else(|_| pyo3::intern!(py, "").clone().into_any());
            _ = base_scope.set_item(intern!(py, "root_path"), root_path);
            let url = PyURL::from_scope(&base_scope)?;
            Py::new(py, url).map(Py::into_any)
        } else {
            let url = PyURL::from_scope(scope)?;
            Py::new(py, url).map(Py::into_any)
        }
    }

    #[getter]
    pub fn headers(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        if let Some(headers) = self.headers_cache.get() {
            return Ok(headers.clone_ref(py));
        }
        if let Ok(headers_cls) = STARLETTE_HEADERS.get(py) {
            let kwargs = PyDict::new(py);
            kwargs.set_item(intern!(py, "scope"), &self.scope)?;
            if let Ok(h) = headers_cls.call((), Some(&kwargs)) {
                return Ok(self.headers_cache.get_or_init(|| h.unbind()).clone_ref(py));
            }
        }
        match scope.get_item(intern!(py, "headers")) {
            Ok(h) => Ok(h.into()),
            Err(_) => Ok(PyDict::new(py).into()),
        }
    }

    #[getter]
    pub fn query_params(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        if let Some(params) = self.query_params_cache.get() {
            return Ok(params.clone_ref(py));
        }
        if let Ok(qp_cls) = STARLETTE_QUERY_PARAMS.get(py) {
            let query_string = scope
                .get_item(intern!(py, "query_string"))
                .unwrap_or_else(|_| PyBytes::new(py, b"").into_any());
            if let Ok(qp) = qp_cls.call1((query_string,)) {
                return Ok(self
                    .query_params_cache
                    .get_or_init(|| qp.unbind())
                    .clone_ref(py));
            }
        }
        match scope.get_item(intern!(py, "query_params")) {
            Ok(params) => Ok(params.into()),
            Err(_) => Ok(PyDict::new(py).into()),
        }
    }

    #[getter]
    pub fn path_params(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        Ok(scope
            .get_item(intern!(py, "path_params"))
            .map_or_else(|_| PyDict::new(py).into(), Into::into))
    }

    #[getter]
    pub fn cookies<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let scope = self.scope.bind(py);

        if let Some(cookies) = self.cookies_cache.get() {
            return Ok(cookies.bind(py).clone().cast_into()?);
        }

        if let Ok(cookies) = scope.get_item(intern!(py, "cookies")) {
            return Ok(cookies.cast_into()?);
        }

        if let Ok(headers) = self.headers(py)
            && let Ok(cookie_val) = headers
                .bind(py)
                .call_method1(pyo3::intern!(py, "get"), ("cookie",))
            && let Ok(cookie_str) = cookie_val.extract::<&str>()
        {
            let simple_cookie = SIMPLE_COOKIE.get(py)?;
            let cookie_obj = simple_cookie.call1((cookie_str,))?;

            let dict = PyDict::new(py);

            let items = cookie_obj.call_method0(intern!(py, "items"))?;
            for item in items.try_iter()? {
                let item = item?;
                let (k, v) = item.extract::<(&str, Bound<'_, PyAny>)>()?;
                let val = v.getattr(intern!(py, "value"))?;
                dict.set_item(k, val)?;
            }

            _ = self.cookies_cache.set(dict.clone().into_any().unbind());
            return Ok(dict);
        }

        Ok(PyDict::new(py))
    }

    #[getter]
    pub fn client(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        scope.get_item(intern!(py, "client")).map_or_else(
            |_| Ok(py.None()),
            |client| {
                if client.is_none() {
                    return Ok(client.unbind());
                }
                Ok(ADDRESS.get(py)?.call1(client.cast::<PyTuple>()?)?.unbind())
            },
        )
    }

    #[getter]
    pub fn session(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let session = self
            .scope
            .bind(py)
            .get_item(intern!(py, "session"))
            .map_err(|_| {
                PyAssertionError::new_err(
                    "SessionMiddleware must be installed to access request.session",
                )
            })?;
        if session.hasattr(intern!(py, "mark_accessed"))? {
            session.call_method0(intern!(py, "mark_accessed"))?;
        }
        Ok(session.unbind())
    }

    #[getter]
    pub fn auth(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.scope
            .bind(py)
            .get_item(intern!(py, "auth"))
            .map(Into::into)
            .map_err(|_| {
                PyAssertionError::new_err(
                    "AuthenticationMiddleware must be installed to access request.auth",
                )
            })
    }

    #[getter]
    pub fn user(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.scope
            .bind(py)
            .get_item(intern!(py, "user"))
            .map(Into::into)
            .map_err(|_| {
                PyAssertionError::new_err(
                    "AuthenticationMiddleware must be installed to access request.user",
                )
            })
    }

    #[getter]
    pub fn state(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        if let Ok(state) = scope.get_item(intern!(py, "_state")) {
            return Ok(state.into());
        }

        if let Ok(state_cls) = STARLETTE_STATE.get(py) {
            if !scope.contains("state")? {
                scope.set_item(intern!(py, "state"), PyDict::new(py))?;
            }
            let state_dict = scope.get_item(intern!(py, "state"))?;
            if let Ok(st) = state_cls.call1((state_dict,)) {
                _ = scope.set_item(intern!(py, "_state"), &st);
                return Ok(st.into());
            }
        }

        if !scope.contains("state")? {
            let namespace = TYPES
                .get(py)?
                .call_method0(intern!(py, "SimpleNamespace"))?;
            scope.set_item(intern!(py, "state"), namespace)?;
        }
        Ok(scope.get_item(intern!(py, "state"))?.into())
    }

    #[pyo3(signature = (name, **path_params))]
    pub fn url_for(
        &self,
        py: Python<'_>,
        name: &str,
        path_params: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        let provider = scope
            .get_item(intern!(py, "router"))
            .or_else(|_| scope.get_item(intern!(py, "app")))
            .map_err(|_| {
                PyRuntimeError::new_err(
                    "The `url_for` method can only be used inside a Starlette application or with a router.",
                )
            })?;

        let url_path = if let Some(params) = path_params {
            provider.call_method(intern!(py, "url_path_for"), (name,), Some(params))?
        } else {
            provider.call_method1(pyo3::intern!(py, "url_path_for"), (name,))?
        };

        let base_url = self.base_url(py)?;
        let kwargs = PyDict::new(py);
        kwargs.set_item(intern!(py, "base_url"), base_url)?;
        let abs_url = url_path.call_method(intern!(py, "make_absolute_url"), (), Some(&kwargs))?;
        Ok(abs_url.into())
    }

    fn __getitem__(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        Ok(scope.get_item(key)?.into())
    }

    fn __setitem__(
        &self,
        py: Python<'_>,
        key: &Bound<'_, PyAny>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let scope = self.scope.bind(py);
        scope.set_item(key, value)
    }

    fn __delitem__(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<()> {
        let scope = self.scope.bind(py);
        scope.del_item(key)
    }

    fn __iter__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        Ok(scope.call_method0(intern!(py, "__iter__"))?.into())
    }

    fn __len__(&self, py: Python<'_>) -> PyResult<usize> {
        let scope = self.scope.bind(py);
        scope.len()
    }

    fn __contains__(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<bool> {
        let scope = self.scope.bind(py);
        scope.contains(key)
    }

    #[pyo3(signature = (key, default=None))]
    fn get(
        &self,
        py: Python<'_>,
        key: &Bound<'_, PyAny>,
        default: Option<Py<PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        if scope.contains(key)? {
            Ok(scope.get_item(key)?.into())
        } else {
            Ok(default.unwrap_or_else(|| py.None()))
        }
    }

    fn keys(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        Ok(scope.call_method0(intern!(py, "keys"))?.into())
    }

    fn values(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        Ok(scope.call_method0(intern!(py, "values"))?.into())
    }

    fn items(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        Ok(scope.call_method0(intern!(py, "items"))?.into())
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        let scope = self.scope.bind(py);
        let type_str: String = scope
            .get_item(intern!(py, "type"))
            .and_then(|t| t.extract())
            .unwrap_or_default();
        let path_str: String = scope
            .get_item(intern!(py, "path"))
            .and_then(|p| p.extract())
            .unwrap_or_default();
        let method_str: String = scope
            .get_item(intern!(py, "method"))
            .and_then(|m| m.extract())
            .unwrap_or_default();

        if method_str.is_empty() {
            format!("HTTPConnection(type={type_str:?}, path={path_str:?})")
        } else {
            format!("HTTPConnection(type={type_str:?}, method={method_str:?}, path={path_str:?})")
        }
    }

    const fn __bool__(&self) -> bool {
        true
    }
}

fn extract_content_length(scope: &Bound<'_, PyAny>) -> Option<usize> {
    scope
        .get_item(intern!(scope.py(), "headers"))
        .ok()?
        .try_iter()
        .ok()?
        .find_map(|header| {
            let h = header.ok()?;
            let tuple = h.cast::<PyTuple>().ok()?;

            let key_item = tuple.get_item(0).ok()?;
            let key: &[u8] = key_item.extract().ok()?;

            if key.eq_ignore_ascii_case(b"content-length") {
                let val_item = tuple.get_item(1).ok()?;
                let val: &[u8] = val_item.extract().ok()?;
                from_utf8(val).ok()?.parse::<usize>().ok()
            } else {
                None
            }
        })
}

fn process_asgi_message(message: &Bound<'_, PyAny>, full_body: &mut Vec<u8>) -> PyResult<bool> {
    let typ_item = message.get_item(intern!(message.py(), "type"))?;
    let typ: &str = typ_item.extract()?;

    if typ != "http.request" {
        return Ok(false);
    }

    if let Ok(body_item) = message.get_item(intern!(message.py(), "body")) {
        if let Ok(py_bytes) = body_item.cast::<PyBytes>() {
            full_body.extend_from_slice(py_bytes.as_bytes());
        } else {
            let bytes: Vec<u8> = body_item.extract()?;
            full_body.extend_from_slice(&bytes);
        }
    }

    let more_body = message
        .get_item(intern!(message.py(), "more_body"))
        .ok()
        .and_then(|m| m.extract::<bool>().ok())
        .unwrap_or(false);

    Ok(!more_body)
}

/// Gives each request a shallow copy of the lifespan state, like uvicorn.
fn request_state<'py>(state: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    match state.cast::<PyDict>() {
        Ok(state) => state.copy().map(Bound::into_any),
        Err(_) => state.call_method0(intern!(state.py(), "copy")),
    }
}

pub fn create_py_request(
    py: Python<'_>,
    input: &RequestInput<'_>,
    raw_body: Option<Bytes>,
) -> PyResult<Py<PyAny>> {
    let headers = Arc::new(input.headers.clone());
    let scope = PyDict::new(py);
    scope.set_item(intern!(py, "type"), intern!(py, "http"))?;
    scope.set_item(intern!(py, "method"), input.method)?;
    let mount = input.extensions.get::<AsgiMount>();
    let raw_path = mount.map_or(input.path, |mount| &mount.path);
    let path = decode_path(raw_path);
    if let Cow::Owned(_) = path {
        scope.set_item(
            intern!(py, "raw_path"),
            PyBytes::new(py, raw_path.as_bytes()),
        )?;
    }
    scope.set_item(intern!(py, "path"), path)?;
    if let Some(mount) = mount {
        scope.set_item(intern!(py, "root_path"), &*mount.root_path)?;
        if let Some(app_root_path) = &mount.app_root_path {
            scope.set_item(intern!(py, "app_root_path"), app_root_path)?;
        }
        if let Some(router) = &mount.router {
            scope.set_item(intern!(py, "router"), router)?;
        }
    }
    scope.set_item(intern!(py, "query_string"), input.query_string)?;
    scope.set_item(
        intern!(py, "headers"),
        Bound::new(py, PyScopeHeaders::new(Arc::clone(&headers)))?,
    )?;
    if let Some(server) = input.server.or_else(current_server) {
        let server = server.server();
        if let Some(app) = server.app() {
            scope.set_item(intern!(py, "app"), app.bind(py))?;
        }
        if let Some(state) = server.lifespan_state(py) {
            scope.set_item(intern!(py, "state"), request_state(state.bind(py))?)?;
        }
    }

    let path_params = PyDict::new(py);
    if let Some(params) = input.path_params.get() {
        params
            .iter()
            .try_for_each(|(k, v)| path_params.set_item(*k, decode_path(v)))?;
    }
    scope.set_item(intern!(py, "path_params"), path_params)?;
    if let Some(client) = input.client() {
        scope.set_item(intern!(py, "client"), client)?;
    }
    if let Some(SessionHandle(slot)) = input.extensions.get::<SessionHandle>() {
        scope.set_item(intern!(py, "session"), slot.session(py)?)?;
    }

    let bound_req = PyRequest::create_bound(
        py,
        scope.into_any().unbind(),
        Some(headers),
        raw_body,
        input.server_addr(),
    )?;
    Ok(bound_req.into_any().unbind())
}

#[pyclass(
    frozen,
    name = "ScopeHeaders",
    module = "fastrapi.request",
    skip_from_py_object
)]
pub struct PyScopeHeaders {
    headers: Arc<HeaderMap>,
}

impl PyScopeHeaders {
    #[must_use]
    pub const fn new(headers: Arc<HeaderMap>) -> Self {
        Self { headers }
    }

    fn pair<'py>(
        py: Python<'py>,
        name: &axum::http::HeaderName,
        value: &axum::http::HeaderValue,
    ) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(
            py,
            [
                PyBytes::new(py, name.as_str().as_bytes()),
                PyBytes::new(py, value.as_bytes()),
            ],
        )
    }

    fn to_list<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for (name, value) in self.headers.iter() {
            list.append(Self::pair(py, name, value)?)?;
        }
        Ok(list)
    }
}

#[pymethods]
impl PyScopeHeaders {
    fn __len__(&self) -> usize {
        self.headers.len()
    }

    fn __getitem__<'py>(&self, py: Python<'py>, index: isize) -> PyResult<Bound<'py, PyTuple>> {
        let len = self.headers.len().cast_signed();
        let position = if index < 0 { index + len } else { index };
        usize::try_from(position)
            .ok()
            .and_then(|position| self.headers.iter().nth(position))
            .ok_or_else(|| pyo3::exceptions::PyIndexError::new_err("header index out of range"))
            .and_then(|(name, value)| Self::pair(py, name, value))
    }

    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, pyo3::types::PyIterator>> {
        self.to_list(py)?.into_any().try_iter()
    }

    fn __eq__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<bool> {
        self.to_list(py)?.eq(other)
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(self.to_list(py)?.repr()?.to_string())
    }
}

pub fn create_stub_request(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let scope = PyDict::new(py);
    scope.set_item(intern!(py, "type"), intern!(py, "http"))?;
    let bound_req = PyRequest::create_bound(py, scope.into_any().unbind(), None, None, None)?;
    Ok(bound_req.into_any().unbind())
}

/// A base class for incoming HTTP connections, that is used to provide
/// any functionality that is common to both `Request` and `WebSocket`.
#[pyclass(
    extends = PyHTTPConnection,
    subclass,
    name = "Request",
    module = "fastrapi.request",
    skip_from_py_object
)]
#[derive(Clone)]
pub struct PyRequest {
    #[pyo3(get, set)]
    pub send: Py<PyAny>,
    pub(crate) body_cache: Arc<OnceCell<Bytes>>,
    pub(crate) raw_headers: Option<Arc<HeaderMap>>,
}

impl PyRequest {
    pub fn create_bound(
        py: Python<'_>,
        scope: Py<PyAny>,
        headers: Option<Arc<HeaderMap>>,
        raw_body: Option<Bytes>,
        server: Option<SocketAddr>,
    ) -> PyResult<Bound<'_, Self>> {
        let conn = PyHTTPConnection::from_scope(scope, py.None()).with_server(server);
        let req = Self {
            send: py.None(),
            body_cache: Arc::new(OnceCell::new()),
            raw_headers: headers,
        };

        if let Some(bytes) = raw_body {
            _ = req.body_cache.set(bytes);
        }

        let initializer = PyClassInitializer::from(conn).add_subclass(req);
        Bound::new(py, initializer)
    }

    fn starlette_headers<'py>(
        &self,
        py: Python<'py>,
        conn: &PyHTTPConnection,
    ) -> PyResult<Bound<'py, PyAny>> {
        if let Some(headers) = conn.headers_cache.get() {
            return Ok(headers.bind(py).clone());
        }

        let raw_list = match &self.raw_headers {
            Some(headers) => {
                let list = PyList::empty(py);
                for (key, value) in headers.iter() {
                    list.append((
                        PyBytes::new(py, key.as_str().as_bytes()),
                        PyBytes::new(py, value.as_bytes()),
                    ))?;
                }
                list
            }
            None => PyList::empty(py),
        };

        let headers_cls = STARLETTE_HEADERS.get(py)?;
        let kwargs = PyDict::new(py);
        kwargs.set_item(intern!(py, "raw"), &raw_list)?;
        let headers_obj = headers_cls.call((), Some(&kwargs))?;
        Ok(conn
            .headers_cache
            .get_or_init(|| headers_obj.unbind())
            .bind(py)
            .clone())
    }

    fn read_body<'py>(
        &self,
        py: Python<'py>,
        conn: &PyHTTPConnection,
    ) -> PyResult<Bound<'py, PyAny>> {
        if let Some(bytes) = self.body_cache.get() {
            return ready(py, PyBytes::new(py, bytes).into_any());
        }

        let scope_any: &Bound<'py, PyAny> = conn.scope.bind(py).as_any();
        let method_item = scope_any.get_item(intern!(py, "method")).ok();
        let method: &str = method_item
            .as_ref()
            .and_then(|m| m.extract::<&str>().ok())
            .unwrap_or("");

        let is_write_method = method.eq_ignore_ascii_case("POST")
            || method.eq_ignore_ascii_case("PUT")
            || method.eq_ignore_ascii_case("PATCH")
            || method.eq_ignore_ascii_case("DELETE");

        if !is_write_method || conn.receive.bind(py).is_none() {
            return ready(py, PyBytes::new(py, &[]).into_any());
        }

        let content_length = extract_content_length(scope_any);
        let receive = conn.receive.clone();
        let body_cell = self.body_cache.clone();
        let locals = py_bridge::get_current_locals(py)?;

        py_bridge::future_into_py_with_locals(py, locals.clone(), async move {
            let body: Bytes = body_cell
                .get_or_try_init(|| async {
                    let mut full_body = content_length.map_or_else(Vec::new, Vec::with_capacity);

                    loop {
                        let fut = Python::attach(|py| -> PyResult<_> {
                            let awaitable = receive.bind(py).call0()?;
                            py_bridge::into_future_with_locals(&locals, awaitable)
                        })?;

                        let message = fut.await?;

                        let done = Python::attach(|py| {
                            process_asgi_message(message.bind(py), &mut full_body)
                        })?;

                        if done {
                            break;
                        }
                    }
                    Ok::<Bytes, PyErr>(full_body.into())
                })
                .await?
                .clone();
            Python::attach(|py| Ok(PyBytes::new(py, &body).into_any().unbind()))
        })
    }
}

#[pyclass(frozen, name = "_ReadyAwaitable", module = "fastrapi.requests")]
struct ReadyAwaitable {
    value: Py<PyAny>,
}

#[pymethods]
impl ReadyAwaitable {
    const fn __await__(slf: Bound<'_, Self>) -> Bound<'_, Self> {
        slf
    }

    const fn __iter__(slf: Bound<'_, Self>) -> Bound<'_, Self> {
        slf
    }

    fn __next__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        Err(pyo3::exceptions::PyStopIteration::new_err((self
            .value
            .clone_ref(py),)))
    }

    #[pyo3(signature = (_value=None))]
    fn send(&self, py: Python<'_>, _value: Option<&Bound<'_, PyAny>>) -> PyResult<Py<PyAny>> {
        self.__next__(py)
    }
}

#[pyclass(frozen, name = "_BodyReceive", module = "fastrapi.requests")]
struct BodyReceive {
    body: Arc<OnceCell<Bytes>>,
    delivered: std::sync::atomic::AtomicBool,
}

#[pymethods]
impl BodyReceive {
    fn __call__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let message = PyDict::new(py);
        if self
            .delivered
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            message.set_item(intern!(py, "type"), intern!(py, "http.disconnect"))?;
        } else {
            let body = self.body.get().map_or(&b""[..], |body| body);
            message.set_item(intern!(py, "type"), intern!(py, "http.request"))?;
            message.set_item(intern!(py, "body"), PyBytes::new(py, body))?;
            message.set_item(intern!(py, "more_body"), false)?;
        }
        ready(py, message.into_any())
    }
}

fn ready<'py>(py: Python<'py>, value: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    Ok(Bound::new(
        py,
        ReadyAwaitable {
            value: value.unbind(),
        },
    )?
    .into_any())
}

#[pymethods]
impl PyRequest {
    #[getter]
    fn receive(slf: &Bound<'_, Self>) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let receive = slf.as_super().borrow().receive.clone_ref(py);
        if !receive.is_none(py) {
            return Ok(receive);
        }
        let body = Arc::clone(&slf.borrow().body_cache);
        Py::new(
            py,
            BodyReceive {
                body,
                delivered: std::sync::atomic::AtomicBool::new(false),
            },
        )
        .map(Py::into_any)
    }

    #[new]
    #[pyo3(signature = (scope, receive=None, send=None))]
    fn new(
        py: Python<'_>,
        scope: Py<PyAny>,
        receive: Option<Py<PyAny>>,
        send: Option<Py<PyAny>>,
    ) -> PyResult<PyClassInitializer<Self>> {
        let conn = PyHTTPConnection::new(py, scope, receive)?;
        let req = Self {
            send: send.unwrap_or_else(|| py.None()),
            body_cache: Arc::new(OnceCell::new()),
            raw_headers: None,
        };
        Ok(PyClassInitializer::from(conn).add_subclass(req))
    }

    #[getter]
    pub fn method<'py>(self_: &Bound<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let conn: &Bound<'py, PyHTTPConnection> = self_.as_super();
        conn.borrow().scope.bind(py).get_item(intern!(py, "method"))
    }

    #[getter]
    pub fn headers<'py>(self_: &Bound<'_, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let conn: &Bound<'_, PyHTTPConnection> = self_.as_super();
        self_.borrow().starlette_headers(py, &conn.borrow())
    }

    #[getter]
    pub fn cookies<'py>(self_: &Bound<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let conn = self_.as_super().borrow();
        if let Some(cookies) = conn.cookies_cache.get() {
            return Ok(cookies.bind(py).clone().cast_into()?);
        }

        let dict = PyDict::new(py);
        if let Some(headers) = &self_.borrow().raw_headers {
            for header_value in headers.get_all(COOKIE) {
                let Ok(raw) = header_value.to_str() else {
                    continue;
                };
                for parsed in Cookie::split_parse(raw) {
                    let Ok(cookie) = parsed else { continue };
                    if let (Some(name), Some(value)) = (cookie.name_raw(), cookie.value_raw()) {
                        dict.set_item(name, value)?;
                    }
                }
            }
        }

        _ = conn.cookies_cache.set(dict.clone().into_any().unbind());
        Ok(dict)
    }

    fn body<'py>(self_: &Bound<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let conn: &Bound<'py, PyHTTPConnection> = self_.as_super();
        let conn_borrow = conn.borrow();
        let req_borrow = self_.borrow();
        req_borrow.read_body(py, &conn_borrow)
    }

    fn json<'py>(self_: &Bound<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let conn: &Bound<'py, PyHTTPConnection> = self_.as_super();
        let conn_borrow = conn.borrow();
        let req_borrow = self_.borrow();
        let body_awaitable = req_borrow.read_body(py, &conn_borrow)?;
        let locals = py_bridge::get_current_locals(py)?;
        let body_fut = py_bridge::into_future_with_locals(&locals, body_awaitable)?;
        py_bridge::future_into_py_with_locals(py, locals, async move {
            let body_bytes: Py<PyAny> = body_fut.await?;

            Python::attach(|py| {
                let bytes = body_bytes.bind(py);
                let obj = JSON_MODULE
                    .get(py)?
                    .call_method1(pyo3::intern!(py, "loads"), (bytes,))?;
                Ok(obj.unbind())
            })
        })
    }

    #[pyo3(signature = (*, max_files=1000.0, max_fields=1000.0, max_part_size=1024 * 1024))]
    fn form<'py>(
        self_: &Bound<'py, Self>,
        py: Python<'py>,
        max_files: f64,
        max_fields: f64,
        max_part_size: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        let limits = form::FormLimits {
            files: max_files,
            fields: max_fields,
            part_size: max_part_size,
        };
        let conn: &Bound<'py, PyHTTPConnection> = self_.as_super();
        let conn_borrow = conn.borrow();
        let req_borrow = self_.borrow();
        let body_awaitable = req_borrow.read_body(py, &conn_borrow)?;
        let content_type = form::content_type_of(py, conn_borrow.scope.bind(py));
        let locals = py_bridge::get_current_locals(py)?;
        let body_fut = py_bridge::into_future_with_locals(&locals, body_awaitable)?;
        let body_cell = req_borrow.body_cache.clone();

        let parsed = py_bridge::future_into_py_with_locals(py, locals, async move {
            let body_bytes: Py<PyAny> = body_fut.await?;

            let raw: Bytes = body_cell.get().cloned().unwrap_or_else(|| {
                Python::attach(|py| {
                    body_bytes
                        .bind(py)
                        .cast::<PyBytes>()
                        .map(fastrapi_pyutil::body::bytes_body)
                        .unwrap_or_default()
                })
            });

            let entries = match form::parse_form(&content_type, raw, limits).await {
                Ok(entries) => entries,
                Err(message) => {
                    return Python::attach(|py| Err(http_exception(py, 400, Some(&message), None)));
                }
            };
            Python::attach(|py| form::to_form_data(py, entries))
        })?;
        Bound::new(py, form::PyFormCall::new(parsed)).map(Bound::into_any)
    }

    fn form_sync<'py>(self_: &Bound<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let conn: &Bound<'py, PyHTTPConnection> = self_.as_super();
        let request = self_.borrow();
        let raw = request.body_cache.get().cloned().unwrap_or_default();
        let content_type = form::content_type_of(py, conn.borrow().scope.bind(py));
        let parsed = form::parse_form(&content_type, raw, form::FormLimits::default())
            .now_or_never()
            .ok_or_else(|| PyRuntimeError::new_err("the form parser did not finish"))?
            .map_err(|message| http_exception(py, 400, Some(&message), None))?;
        form::to_form_data(py, parsed).map(|value| value.bind(py).clone())
    }

    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        py_bridge::future_into_py(py, async move { Ok(()) })
    }

    fn is_disconnected<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        py_bridge::future_into_py(py, async move { Ok(false) })
    }

    fn stream<'py>(self_: &Bound<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let conn = self_.as_super();
        let request = py
            .import("starlette.requests")?
            .getattr(pyo3::intern!(py, "Request"))?;
        let kwargs = PyDict::new(py);
        kwargs.set_item(
            pyo3::intern!(py, "receive"),
            conn.borrow().receive.clone_ref(py),
        )?;
        kwargs.set_item(pyo3::intern!(py, "send"), self_.borrow().send.clone_ref(py))?;
        let starlette = request.call((conn.borrow().scope.clone_ref(py),), Some(&kwargs))?;
        if let Some(body) = self_.borrow().body_cache.get() {
            starlette.setattr(pyo3::intern!(py, "_body"), PyBytes::new(py, body.as_ref()))?;
        }
        starlette.call_method0(pyo3::intern!(py, "stream"))
    }

    fn send_push_promise<'py>(
        self_: &Bound<'py, Self>,
        py: Python<'py>,
        path: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let conn = self_.as_super();
        let request = py
            .import("starlette.requests")?
            .getattr(pyo3::intern!(py, "Request"))?;
        let kwargs = PyDict::new(py);
        kwargs.set_item(
            pyo3::intern!(py, "receive"),
            conn.borrow().receive.clone_ref(py),
        )?;
        kwargs.set_item(pyo3::intern!(py, "send"), self_.borrow().send.clone_ref(py))?;
        let starlette = request.call((conn.borrow().scope.clone_ref(py),), Some(&kwargs))?;
        starlette.call_method1(pyo3::intern!(py, "send_push_promise"), (path,))
    }

    fn __repr__(self_: &Bound<'_, Self>) -> String {
        let conn: &Bound<'_, PyHTTPConnection> = self_.as_super();
        let scope = conn.borrow().scope.bind(self_.py()).clone();
        let method: String = scope
            .get_item(intern!(self_.py(), "method"))
            .and_then(|m| m.extract())
            .unwrap_or_else(|_| String::new());
        let path: String = scope
            .get_item(intern!(self_.py(), "path"))
            .and_then(|p| p.extract())
            .unwrap_or_else(|_| String::new());
        format!("Request(method={method:?}, path={path:?})")
    }
}
