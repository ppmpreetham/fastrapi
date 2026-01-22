use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict};
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
        let receive = self.receive.clone();
        let body_store = self._body.clone();
        let is_consumed = self._is_consumed.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            // cached
            {
                let lock = body_store.lock().unwrap();
                if let Some(data) = &*lock {
                    return Python::attach(|py| Ok(PyBytes::new(py, data).unbind()));
                }
            }

            // consumed but not cached
            {
                let lock = is_consumed.lock().unwrap();
                if *lock {
                    return Err(pyo3::exceptions::PyRuntimeError::new_err(
                        "Stream already consumed",
                    ));
                }
            }

            // ASGI Read Loop
            let mut data = Vec::new();
            loop {
                let fut = Python::attach(|py| {
                    let awaitable = receive.bind(py).call0()?;
                    pyo3_async_runtimes::tokio::into_future(awaitable)
                })?;

                let message_obj: Py<PyAny> = fut.await?;

                let (chunk, more_body) = Python::attach(|py| -> PyResult<(Vec<u8>, bool)> {
                    let msg = message_obj.bind(py);
                    let msg_type: String = msg.get_item("type")?.extract()?;

                    if msg_type != "http.request" {
                        return Ok((vec![], false));
                    }

                    let body_bytes: Vec<u8> = match msg.get_item("body") {
                        Ok(b) => b.extract()?,
                        Err(_) => vec![],
                    };

                    let more = match msg.get_item("more_body") {
                        Ok(m) => m.extract()?,
                        Err(_) => false,
                    };

                    Ok((body_bytes, more))
                })?;

                data.extend_from_slice(&chunk);
                if !more_body {
                    break;
                }
            }

            // cache
            {
                let mut lock = body_store.lock().unwrap();
                *lock = Some(data.clone());
            }
            {
                let mut lock = is_consumed.lock().unwrap();
                *lock = true;
            }

            Python::attach(|py| Ok(PyBytes::new(py, &data).unbind()))
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
