use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict};
use std::sync::Arc;
use tokio::sync::OnceCell;

#[pyclass(name = "Request", module = "fastrapi.request", skip_from_py_object)]
#[derive(Clone)]
pub struct PyRequest {
    #[pyo3(get)]
    pub scope: Py<PyAny>,
    #[pyo3(get)]
    pub receive: Py<PyAny>,
    #[pyo3(get)]
    pub send: Py<PyAny>,

    // Cache for the body if it has been read once
    _body: Arc<OnceCell<Arc<[u8]>>>,
}

impl PyRequest {
    pub fn from_scope(py: Python<'_>, scope: Py<PyAny>) -> Self {
        Self {
            scope,
            receive: py.None(),
            send: py.None(),
            _body: Arc::new(OnceCell::new()),
        }
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
    ) -> PyResult<Self> {
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
            send: send.unwrap_or_else(|| py.None()),
            _body: Arc::new(OnceCell::new()),
        })
    }

    #[getter]
    fn client(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        match scope.get_item("client") {
            Ok(client) => Ok(client.into()),
            Err(_) => Ok(py.None()),
        }
    }

    #[getter]
    fn state(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);

        if !scope.contains("state")? {
            let types = py.import("types")?;
            let namespace = types.call_method0("SimpleNamespace")?;
            scope.set_item("state", namespace)?;
        }

        Ok(scope.get_item("state")?.into())
    }

    #[getter]
    fn headers(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        scope.get_item("headers").map(|h| h.into())
    }

    #[getter]
    fn path_params(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        match scope.get_item("path_params") {
            Ok(params) => Ok(params.into()),
            Err(_) => Ok(PyDict::new(py).into()),
        }
    }

    #[getter]
    fn query_params(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        match scope.get_item("query_params") {
            Ok(params) => Ok(params.into()),
            Err(_) => Ok(PyDict::new(py).into()),
        }
    }

    #[getter]
    fn cookies(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope = self.scope.bind(py);
        match scope.get_item("cookies") {
            Ok(cookies) => Ok(cookies.into()),
            Err(_) => Ok(PyDict::new(py).into()),
        }
    }

    fn body<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let scope_bound = self.scope.bind(py);
        let method_opt = scope_bound.get_item("method").ok();
        let method = method_opt
            .and_then(|m| m.extract::<String>().ok())
            .unwrap_or_default()
            .to_ascii_uppercase();
        if !matches!(method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE") {
            return Ok(PyBytes::new(py, &[]).into_any());
        }

        let receive = self.receive.clone();
        let body_cell = self._body.clone();
        let content_length = Python::attach(|py| -> Option<usize> {
            let scope = self.scope.bind(py);
            let headers = scope.get_item("headers").ok()?;
            for header in headers.try_iter().ok()? {
                let tuple = header.ok()?;
                let key_obj = tuple.get_item(0).ok()?;
                let key: &[u8] = key_obj.extract().ok()?;
                if key.eq_ignore_ascii_case(b"content-length") {
                    let val_obj = tuple.get_item(1).ok()?;
                    let val: &[u8] = val_obj.extract().ok()?;
                    return std::str::from_utf8(val).ok()?.parse().ok();
                }
            }
            None
        });

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let body: Arc<[u8]> = body_cell
                .get_or_try_init(|| async {
                    let mut full_body = if let Some(len) = content_length {
                        Vec::with_capacity(len)
                    } else {
                        Vec::new()
                    };
                    loop {
                        let message = {
                            let fut = Python::attach(|py| {
                                let awaitable = receive.bind(py).call0()?;
                                pyo3_async_runtimes::tokio::into_future(awaitable)
                            })?;

                            fut.await?
                        };
                        let mut done = false;
                        Python::attach(|py| -> PyResult<()> {
                            let msg = message.bind(py);
                            let typ: String = msg.get_item("type")?.extract()?;
                            if typ != "http.request" {
                                return Ok(());
                            }
                            if let Ok(body_item) = msg.get_item("body") {
                                let bytes: Vec<u8> = body_item.extract()?;
                                full_body.extend_from_slice(&bytes);
                            }
                            let more_body: bool =
                                msg.get_item("more_body")?.extract().unwrap_or(false);
                            if !more_body {
                                done = true;
                            }
                            Ok(())
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

    fn json<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let body_awaitable = self.body(py)?;
        let body_fut = pyo3_async_runtimes::tokio::into_future(body_awaitable)?;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let body_bytes: Py<PyAny> = body_fut.await?;

            Python::attach(|py| {
                let bytes = body_bytes.bind(py);
                let json_mod = py.import("json")?;
                let obj = json_mod.call_method1("loads", (bytes,))?;
                Ok(obj.unbind())
            })
        })
    }
}

#[pyclass(name = "HTTPConnection", module = "fastrapi.request")]
pub struct PyHTTPConnection {
    #[pyo3(get)]
    pub scope: Py<PyAny>,
}

#[pymethods]
impl PyHTTPConnection {
    #[new]
    fn new(scope: Py<PyAny>) -> Self {
        Self { scope }
    }
}
