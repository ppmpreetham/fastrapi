use pyo3::exceptions::PyValueError;
use pyo3::types::{PyAny, PyBytes, PyDict};
use pyo3::{prelude::*, IntoPyObjectExt};
use std::sync::{Arc, Mutex};

#[pyclass(name = "Request", module = "fastrapi.request")]
#[derive(Clone)]
pub struct PyRequest {
    #[pyo3(get)]
    pub scope: Py<PyAny>,
    #[pyo3(get)]
    pub receive: Py<PyAny>,
    #[pyo3(get)]
    pub send: Py<PyAny>,

    // Cache for the body if it has been read once
    _body: Arc<Mutex<Option<Vec<u8>>>>,
    _is_consumed: Arc<Mutex<bool>>,
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
            _body: Arc::new(Mutex::new(None)),
            _is_consumed: Arc::new(Mutex::new(false)),
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
        let cache = self._body.clone();
        let consumed = self._is_consumed.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            {
                let cache_guard = cache.lock().unwrap();
                if let Some(cached) = &*cache_guard {
                    return Python::attach(|py| Ok(PyBytes::new(py, cached).into_any().unbind()));
                }
            }

            {
                let mut consumed_guard = consumed.lock().unwrap();
                if *consumed_guard {
                    return Err(pyo3::exceptions::PyRuntimeError::new_err(
                        "Body stream already consumed",
                    ));
                }
            }

            let mut full_body = Vec::new();

            loop {
                let message = {
                    let fut = Python::attach(|py| {
                        let awaitable = receive.bind(py).call0()?;
                        pyo3_async_runtimes::tokio::into_future(awaitable)
                    })?;

                    fut.await?
                };

                Python::attach(|py| -> PyResult<()> {
                    let msg = message.bind(py);
                    let typ: String = msg.get_item("type")?.extract()?;

                    if typ != "http.request" {
                        return Ok(());
                    }

                    if let Ok(body_item) = msg.get_item("body") {
                        let bytes: Vec<u8> = body_item.extract()?;
                        full_body.extend(bytes);
                    }

                    let more_body: bool = msg.get_item("more_body")?.extract().unwrap_or(false);

                    if !more_body {
                        let mut cache_guard = cache.lock().unwrap();
                        *cache_guard = Some(full_body.clone());

                        let mut consumed_guard = consumed.lock().unwrap();
                        *consumed_guard = true;

                        return Ok(());
                    }

                    Ok(())
                })?;
            }

            unreachable!("Body reading loop should never exit normally");
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

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyRequest>()?;
    m.add_class::<PyHTTPConnection>()?;
    Ok(())
}
