use crate::routing::types::RequestInput;
use pyo3::PyClassInitializer;
use pyo3::exceptions::{PyAssertionError, PyRuntimeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict, PyString, PyTuple};
use std::sync::Arc;
use tokio::sync::OnceCell;

crate::cached_py_import!(STARLETTE_URL, "starlette.datastructures", "URL");
crate::cached_py_import!(STARLETTE_HEADERS, "starlette.datastructures", "Headers");
crate::cached_py_import!(
    STARLETTE_QUERY_PARAMS,
    "starlette.datastructures",
    "QueryParams"
);
crate::cached_py_import!(STARLETTE_STATE, "starlette.datastructures", "State");
crate::cached_py_import!(STARLETTE_FORM_DATA, "starlette.datastructures", "FormData");
crate::cached_py_import!(TYPES_MODULE, "types");
crate::cached_py_import!(HTTP_COOKIES_SIMPLE_COOKIE, "http.cookies", "SimpleCookie");
crate::cached_py_import!(JSON_MODULE, "json");

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
    fn new(scope: Py<PyAny>) -> Self {
        Self { scope }
    }

    #[getter]
    fn state(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        if !scope.contains("state")? {
            let namespace = TYPES_MODULE.get(py)?.call_method0("SimpleNamespace")?;
            scope.set_item("state", namespace)?;
        }
        Ok(scope.get_item("state")?.into())
    }
}

#[pyclass(
    subclass,
    name = "HTTPConnection",
    module = "fastrapi.request",
    skip_from_py_object
)]
#[derive(Clone)]
pub struct PyHTTPConnection {
    #[pyo3(get, set)]
    pub scope: Py<PyAny>,
    #[pyo3(get, set)]
    pub receive: Py<PyAny>,
}

#[pymethods]
impl PyHTTPConnection {
    #[new]
    #[pyo3(signature = (scope, receive=None))]
    pub fn new(py: Python<'_>, scope: Py<PyAny>, receive: Option<Py<PyAny>>) -> PyResult<Self> {
        let scope_bound = scope.bind(py);

        if let Ok(scope_type) = scope_bound.get_item("type") {
            let type_str: String = scope_type.extract()?;
            if type_str != "http" && type_str != "websocket" {
                return Err(PyValueError::new_err(
                    "Scope type must be 'http' or 'websocket'",
                ));
            }
        }

        Ok(Self {
            scope,
            receive: receive.unwrap_or_else(|| py.None()),
        })
    }

    #[getter]
    pub fn app(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        if let Ok(app) = scope.get_item("app")
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
        let url_class = STARLETTE_URL.get(py)?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("scope", &self.scope)?;
        let url = url_class.call((), Some(&kwargs))?;
        Ok(url.into())
    }

    #[getter]
    pub fn base_url(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let url_class = STARLETTE_URL.get(py)?;
        let scope = self.scope.bind(py);

        if let Ok(base_scope) = scope.call_method0("copy") {
            let _ = base_scope.set_item("path", "/");
            let _ = base_scope.set_item("query_string", PyBytes::new(py, b""));
            let root_path = scope
                .get_item("root_path")
                .unwrap_or_else(|_| PyString::new(py, "").into_any());
            let _ = base_scope.set_item("root_path", root_path);
            let kwargs = PyDict::new(py);
            kwargs.set_item("scope", base_scope)?;
            let url = url_class.call((), Some(&kwargs))?;
            Ok(url.into())
        } else {
            let kwargs = PyDict::new(py);
            kwargs.set_item("scope", &self.scope)?;
            let url = url_class.call((), Some(&kwargs))?;
            Ok(url.into())
        }
    }

    #[getter]
    pub fn headers(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        if let Ok(headers) = scope.get_item("_headers") {
            return Ok(headers.into());
        }
        if let Ok(headers_cls) = STARLETTE_HEADERS.get(py) {
            let kwargs = PyDict::new(py);
            kwargs.set_item("scope", &self.scope)?;
            if let Ok(h) = headers_cls.call((), Some(&kwargs)) {
                let _ = scope.set_item("_headers", &h);
                return Ok(h.into());
            }
        }
        match scope.get_item("headers") {
            Ok(h) => Ok(h.into()),
            Err(_) => Ok(PyDict::new(py).into()),
        }
    }

    #[getter]
    pub fn query_params(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        if let Ok(params) = scope.get_item("_query_params") {
            return Ok(params.into());
        }
        if let Ok(qp_cls) = STARLETTE_QUERY_PARAMS.get(py) {
            let query_string = scope
                .get_item("query_string")
                .unwrap_or_else(|_| PyBytes::new(py, b"").into_any());
            let kwargs = PyDict::new(py);
            kwargs.set_item("query_string", query_string)?;
            if let Ok(qp) = qp_cls.call((), Some(&kwargs)) {
                let _ = scope.set_item("_query_params", &qp);
                return Ok(qp.into());
            }
        }
        match scope.get_item("query_params") {
            Ok(params) => Ok(params.into()),
            Err(_) => Ok(PyDict::new(py).into()),
        }
    }

    #[getter]
    pub fn path_params(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        Ok(scope
            .get_item("path_params")
            .map(Into::into)
            .unwrap_or_else(|_| PyDict::new(py).into()))
    }

    #[getter]
    pub fn cookies<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let scope = self.scope.bind(py);

        if let Ok(cookies) = scope.get_item("_cookies") {
            return Ok(cookies.cast_into()?);
        }

        if let Ok(cookies) = scope.get_item("cookies") {
            return Ok(cookies.cast_into()?);
        }

        if let Ok(headers) = self.headers(py)
            && let Ok(cookie_val) = headers.bind(py).call_method1("get", ("cookie",))
            && let Ok(cookie_str) = cookie_val.extract::<&str>()
        {
            let simple_cookie = HTTP_COOKIES_SIMPLE_COOKIE.get(py)?;
            let cookie_obj = simple_cookie.call1((cookie_str,))?;

            let dict = PyDict::new(py);

            let items = cookie_obj.call_method0("items")?;
            for item in items.try_iter()? {
                let item = item?;
                let (k, v) = item.extract::<(&str, Bound<'_, PyAny>)>()?;
                let val = v.getattr("value")?;
                dict.set_item(k, val)?;
            }

            scope.set_item("_cookies", &dict)?;
            return Ok(dict);
        }

        Ok(PyDict::new(py))
    }

    #[getter]
    pub fn client(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        scope
            .get_item("client")
            .map(|client| {
                if let Ok((host, port)) = client.extract::<(String, u16)>() {
                    let client_info = PyClientInfo { host, port };
                    return Ok(Bound::new(py, client_info)?.into_any().unbind());
                }
                Ok(client.into())
            })
            .unwrap_or_else(|_| Ok(py.None()))
    }

    #[getter]
    pub fn session(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.scope
            .bind(py)
            .get_item("session")
            .map(Into::into)
            .map_err(|_| {
                PyAssertionError::new_err(
                    "SessionMiddleware must be installed to access request.session",
                )
            })
    }

    #[getter]
    pub fn auth(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.scope
            .bind(py)
            .get_item("auth")
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
            .get_item("user")
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
        if let Ok(state) = scope.get_item("_state") {
            return Ok(state.into());
        }

        if let Ok(state_cls) = STARLETTE_STATE.get(py) {
            if !scope.contains("state")? {
                scope.set_item("state", PyDict::new(py))?;
            }
            let state_dict = scope.get_item("state")?;
            if let Ok(st) = state_cls.call1((state_dict,)) {
                let _ = scope.set_item("_state", &st);
                return Ok(st.into());
            }
        }

        if !scope.contains("state")? {
            let namespace = TYPES_MODULE.get(py)?.call_method0("SimpleNamespace")?;
            scope.set_item("state", namespace)?;
        }
        Ok(scope.get_item("state")?.into())
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
            .get_item("router")
            .or_else(|_| scope.get_item("app"))
            .map_err(|_| {
                PyRuntimeError::new_err(
                    "The `url_for` method can only be used inside a Starlette application or with a router.",
                )
            })?;

        let url_path = if let Some(params) = path_params {
            provider.call_method("url_path_for", (name,), Some(params))?
        } else {
            provider.call_method1("url_path_for", (name,))?
        };

        let base_url = self.base_url(py)?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("base_url", base_url)?;
        let abs_url = url_path.call_method("make_absolute_url", (), Some(&kwargs))?;
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
        Ok(scope.call_method0("__iter__")?.into())
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
        Ok(scope.call_method0("keys")?.into())
    }

    fn values(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        Ok(scope.call_method0("values")?.into())
    }

    fn items(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        Ok(scope.call_method0("items")?.into())
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        let scope = self.scope.bind(py);
        let type_str: String = scope
            .get_item("type")
            .and_then(|t| t.extract())
            .unwrap_or_default();
        let path_str: String = scope
            .get_item("path")
            .and_then(|p| p.extract())
            .unwrap_or_default();
        let method_str: String = scope
            .get_item("method")
            .and_then(|m| m.extract())
            .unwrap_or_default();

        if method_str.is_empty() {
            Ok(format!(
                "HTTPConnection(type={:?}, path={:?})",
                type_str, path_str
            ))
        } else {
            Ok(format!(
                "HTTPConnection(type={:?}, method={:?}, path={:?})",
                type_str, method_str, path_str
            ))
        }
    }

    fn __bool__(&self) -> bool {
        true
    }
}

fn extract_content_length(scope: &Bound<'_, PyAny>) -> Option<usize> {
    scope
        .get_item("headers")
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
                std::str::from_utf8(val).ok()?.parse::<usize>().ok()
            } else {
                None
            }
        })
}

fn process_asgi_message(message: &Bound<'_, PyAny>, full_body: &mut Vec<u8>) -> PyResult<bool> {
    let typ_item = message.get_item("type")?;
    let typ: &str = typ_item.extract()?;

    if typ != "http.request" {
        return Ok(false);
    }

    if let Ok(body_item) = message.get_item("body") {
        if let Ok(py_bytes) = body_item.cast::<PyBytes>() {
            full_body.extend_from_slice(py_bytes.as_bytes());
        } else {
            let bytes: Vec<u8> = body_item.extract()?;
            full_body.extend_from_slice(&bytes);
        }
    }

    let more_body = message
        .get_item("more_body")
        .ok()
        .and_then(|m| m.extract::<bool>().ok())
        .unwrap_or(false);

    Ok(!more_body)
}

pub(crate) fn create_py_request<'a>(
    py: Python<'_>,
    input: &RequestInput<'_>,
    raw_body: impl Into<Option<&'a [u8]>>,
) -> PyResult<Py<PyAny>> {
    let scope = PyDict::new(py);
    scope.set_item(intern!(py, "type"), intern!(py, "http"))?;
    scope.set_item(intern!(py, "method"), input.method)?;
    scope.set_item(intern!(py, "path"), input.path)?;
    scope.set_item(intern!(py, "query_string"), input.query_string)?;

    let path_params = PyDict::new(py);
    if let Some(params) = input.path_params.get() {
        params
            .iter()
            .try_for_each(|(k, v)| path_params.set_item(k.as_ref(), v))?;
    }
    scope.set_item(intern!(py, "path_params"), path_params)?;

    let bound_req = PyRequest::create_bound(
        py,
        scope.into_any().unbind(),
        Some(Arc::new(input.headers.clone())),
        raw_body,
    )?;
    Ok(bound_req.into_any().unbind())
}

pub(crate) fn create_stub_request(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let scope = PyDict::new(py);
    scope.set_item(intern!(py, "type"), intern!(py, "http"))?;
    let bound_req = PyRequest::create_bound(py, scope.into_any().unbind(), None, None)?;
    Ok(bound_req.into_any().unbind())
}

#[pyclass(
    extends = PyHTTPConnection,
    name = "Request",
    module = "fastrapi.request",
    skip_from_py_object
)]
#[derive(Clone)]
pub struct PyRequest {
    #[pyo3(get, set)]
    pub send: Py<PyAny>,
    pub(crate) _body: Arc<OnceCell<Arc<[u8]>>>,
    pub(crate) raw_headers: Option<Arc<axum::http::HeaderMap>>,
}

impl PyRequest {
    pub fn create_bound<'a>(
        py: Python<'_>,
        scope: Py<PyAny>,
        headers: Option<Arc<axum::http::HeaderMap>>,
        raw_body: impl Into<Option<&'a [u8]>>,
    ) -> PyResult<Bound<'_, PyRequest>> {
        let conn = PyHTTPConnection {
            scope,
            receive: py.None(),
        };
        let req = PyRequest {
            send: py.None(),
            _body: Arc::new(OnceCell::new()),
            raw_headers: headers,
        };

        if let Some(bytes) = raw_body.into() {
            let _ = req._body.set(bytes.into());
        }

        let initializer = PyClassInitializer::from(conn).add_subclass(req);
        Bound::new(py, initializer)
    }

    fn starlette_headers<'py>(
        &self,
        py: Python<'py>,
        scope: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        if let Ok(headers) = scope.get_item("_headers") {
            return Ok(headers);
        }

        let raw_list = match &self.raw_headers {
            Some(headers) => {
                let list = pyo3::types::PyList::empty(py);
                for (key, value) in headers.iter() {
                    list.append((
                        pyo3::types::PyBytes::new(py, key.as_str().as_bytes()),
                        pyo3::types::PyBytes::new(py, value.as_bytes()),
                    ))?;
                }
                list
            }
            None => pyo3::types::PyList::empty(py),
        };

        let headers_cls = STARLETTE_HEADERS.get(py)?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("raw", &raw_list)?;
        let headers_obj = headers_cls.call((), Some(&kwargs))?;
        scope.set_item("_headers", &headers_obj)?;
        Ok(headers_obj.into_any())
    }

    fn read_body<'py>(
        &self,
        py: Python<'py>,
        conn: &PyHTTPConnection,
    ) -> PyResult<Bound<'py, PyAny>> {
        if let Some(bytes) = self._body.get() {
            let payload = PyBytes::new(py, bytes).into_any().unbind();
            return rsloop::rust_async::future_into_py(py, async move { Ok(payload) });
        }

        let scope_any: &Bound<'py, PyAny> = conn.scope.bind(py).as_any();
        let method_item = scope_any.get_item("method").ok();
        let method: &str = method_item
            .as_ref()
            .and_then(|m| m.extract::<&str>().ok())
            .unwrap_or("");

        let is_write_method = method.eq_ignore_ascii_case("POST")
            || method.eq_ignore_ascii_case("PUT")
            || method.eq_ignore_ascii_case("PATCH")
            || method.eq_ignore_ascii_case("DELETE");

        if !is_write_method {
            return Ok(PyBytes::new(py, &[]).into_any());
        }

        let content_length = extract_content_length(scope_any);
        let receive = conn.receive.clone();
        let body_cell = self._body.clone();
        let locals = rsloop::rust_async::get_current_locals(py)?;

        rsloop::rust_async::future_into_py_with_locals(py, locals.clone(), async move {
            let body: Arc<[u8]> = body_cell
                .get_or_try_init(|| async {
                    let mut full_body = content_length.map_or_else(Vec::new, Vec::with_capacity);

                    loop {
                        let fut = Python::attach(|py| -> PyResult<_> {
                            let awaitable = receive.bind(py).call0()?;
                            rsloop::rust_async::into_future_with_locals(&locals, awaitable)
                        })?;

                        let message = fut.await?;

                        let done = Python::attach(|py| {
                            process_asgi_message(message.bind(py), &mut full_body)
                        })?;

                        if done {
                            break;
                        }
                    }
                    Ok::<Arc<[u8]>, PyErr>(full_body.into())
                })
                .await?
                .clone();
            Python::attach(|py| Ok(PyBytes::new(py, &body).into_any().unbind()))
        })
    }
}

#[pymethods]
impl PyRequest {
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
            _body: Arc::new(OnceCell::new()),
            raw_headers: None,
        };
        Ok(PyClassInitializer::from(conn).add_subclass(req))
    }

    #[getter]
    pub fn headers<'py>(self_: &Bound<'_, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let conn: &Bound<'_, PyHTTPConnection> = self_.as_super();
        let scope = conn.borrow().scope.bind(py).clone();
        self_.borrow().starlette_headers(py, &scope)
    }

    #[getter]
    pub fn cookies<'py>(self_: &Bound<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let conn: &Bound<'py, PyHTTPConnection> = self_.as_super();
        let scope = conn.borrow().scope.bind(py).clone();

        if let Ok(cookies) = scope.get_item("_cookies") {
            return Ok(cookies.cast_into()?);
        }

        let dict = PyDict::new(py);
        if let Some(headers) = &self_.borrow().raw_headers {
            for header_value in headers.get_all(axum::http::header::COOKIE) {
                let Ok(raw) = header_value.to_str() else {
                    continue;
                };
                for parsed in cookie::Cookie::split_parse(raw) {
                    let Ok(cookie) = parsed else { continue };
                    if let (Some(name), Some(value)) = (cookie.name_raw(), cookie.value_raw()) {
                        dict.set_item(name, value)?;
                    }
                }
            }
        }

        scope.set_item("_cookies", &dict)?;
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
        let locals = rsloop::rust_async::get_current_locals(py)?;
        let body_fut = rsloop::rust_async::into_future_with_locals(&locals, body_awaitable)?;
        rsloop::rust_async::future_into_py_with_locals(py, locals, async move {
            let body_bytes: Py<PyAny> = body_fut.await?;

            Python::attach(|py| {
                let bytes = body_bytes.bind(py);
                let obj = JSON_MODULE.get(py)?.call_method1("loads", (bytes,))?;
                Ok(obj.unbind())
            })
        })
    }

    fn form<'py>(self_: &Bound<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let conn: &Bound<'py, PyHTTPConnection> = self_.as_super();
        let conn_borrow = conn.borrow();
        let req_borrow = self_.borrow();
        let body_awaitable = req_borrow.read_body(py, &conn_borrow)?;
        let headers_obj = conn_borrow.headers(py)?;
        let locals = rsloop::rust_async::get_current_locals(py)?;
        let body_fut = rsloop::rust_async::into_future_with_locals(&locals, body_awaitable)?;

        rsloop::rust_async::future_into_py_with_locals(py, locals, async move {
            let _body_bytes: Py<PyAny> = body_fut.await?;
            Python::attach(|py| {
                if let Ok(form_cls) = STARLETTE_FORM_DATA.get(py) {
                    let form_obj = form_cls.call0()?;
                    return Ok(form_obj.unbind());
                }
                let _ = headers_obj;
                Ok(PyDict::new(py).into_any().unbind())
            })
        })
    }

    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        rsloop::rust_async::future_into_py(py, async move { Ok(()) })
    }

    fn is_disconnected<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        rsloop::rust_async::future_into_py(py, async move { Ok(false) })
    }

    fn __repr__(self_: &Bound<'_, Self>) -> PyResult<String> {
        let conn: &Bound<'_, PyHTTPConnection> = self_.as_super();
        let scope = conn.borrow().scope.bind(self_.py()).clone();
        let method: String = scope
            .get_item("method")
            .and_then(|m| m.extract())
            .unwrap_or_else(|_| "".to_string());
        let path: String = scope
            .get_item("path")
            .and_then(|p| p.extract())
            .unwrap_or_else(|_| "".to_string());
        Ok(format!("Request(method={:?}, path={:?})", method, path))
    }
}
