use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};

#[pyclass(name = "Request")]
pub struct PyRequest {
    #[pyo3(get)]
    pub scope: Py<PyAny>,
    #[pyo3(get)]
    pub receive: Py<PyAny>,
    #[pyo3(get)]
    pub send: Py<PyAny>,
    #[pyo3(get)]
    pub stream_consumed: bool,
    #[pyo3(get)]
    pub is_disconnected: bool,
    #[pyo3(get)]
    pub form: Option<Py<PyAny>>,
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
        let scope_dict = scope.bind(py);
        let scope_type: String = scope_dict.get_item("type")?.extract()?;

        if scope_type != "http" {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "Scope type must be 'http'.",
            ));
        }

        Ok(Self {
            scope,
            receive: receive.unwrap_or_else(|| py.None()),
            send: send.unwrap_or_else(|| py.None()),
            stream_consumed: false,
            is_disconnected: false,
            form: None,
        })
    }

    fn url_for(
        &self,
        py: Python<'_>,
        name: String,
        path_params: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let scope_dict = self.scope.bind(py);

        // Try to get router, fallback to app
        let router = match scope_dict.get_item("router") {
            Ok(r) => r,
            Err(_) => scope_dict.get_item("app")?,
        };

        let url_path = if let Some(params) = path_params {
            router.call_method("url_path_for", (name,), Some(params))?
        } else {
            router.call_method1("url_path_for", (name,))?
        };

        let base_url = scope_dict.call_method1("get", ("base_url",))?;
        let absolute_url = url_path.call_method1("make_absolute_url", (base_url,))?;

        Ok(absolute_url.into())
    }

    #[getter]
    fn headers(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope_dict = self.scope.bind(py);
        Ok(scope_dict.call_method1("get", ("headers",))?.into())
    }

    #[getter]
    fn query_params(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope_dict = self.scope.bind(py);
        Ok(scope_dict.call_method1("get", ("query_params",))?.into())
    }

    #[getter]
    fn path_params(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope_dict = self.scope.bind(py);
        Ok(scope_dict.call_method1("get", ("path_params",))?.into())
    }

    #[getter]
    fn cookies(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope_dict = self.scope.bind(py);
        Ok(scope_dict.call_method1("get", ("cookies",))?.into())
    }
}

#[pyclass(name = "HTTPConnection")]
pub struct PyHTTPConnection {
    #[pyo3(get)]
    pub scope: Py<PyAny>,
    #[pyo3(get)]
    pub receive: Py<PyAny>,
    #[pyo3(get)]
    pub send: Py<PyAny>,
}

#[pymethods]
impl PyHTTPConnection {
    #[new]
    #[pyo3(signature = (scope, receive=None, send=None))]
    fn new(
        py: Python<'_>,
        scope: Py<PyAny>,
        receive: Option<Py<PyAny>>,
        send: Option<Py<PyAny>>,
    ) -> PyResult<Self> {
        let scope_dict = scope.bind(py);
        let scope_type: String = scope_dict.get_item("type")?.extract()?;

        if scope_type != "http" && scope_type != "websocket" {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "Scope type must be 'http' or 'websocket'.",
            ));
        }

        Ok(Self {
            scope,
            receive: receive.unwrap_or_else(|| py.None()),
            send: send.unwrap_or_else(|| py.None()),
        })
    }

    #[getter]
    fn headers(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope_dict = self.scope.bind(py);
        Ok(scope_dict.call_method1("get", ("headers",))?.into())
    }

    #[getter]
    fn query_params(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope_dict = self.scope.bind(py);
        Ok(scope_dict.call_method1("get", ("query_params",))?.into())
    }

    #[getter]
    fn path_params(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope_dict = self.scope.bind(py);
        Ok(scope_dict.call_method1("get", ("path_params",))?.into())
    }

    #[getter]
    fn cookies(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let scope_dict = self.scope.bind(py);
        Ok(scope_dict.call_method1("get", ("cookies",))?.into())
    }

    fn url_for(
        &self,
        py: Python<'_>,
        name: String,
        path_params: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let scope_dict = self.scope.bind(py);

        let router = match scope_dict.get_item("router") {
            Ok(r) => r,
            Err(_) => scope_dict.get_item("app")?,
        };

        let url_path = if let Some(params) = path_params {
            router.call_method("url_path_for", (name,), Some(params))?
        } else {
            router.call_method1("url_path_for", (name,))?
        };

        let base_url = scope_dict.call_method1("get", ("base_url",))?;
        let absolute_url = url_path.call_method1("make_absolute_url", (base_url,))?;

        Ok(absolute_url.into())
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyRequest>()?;
    m.add_class::<PyHTTPConnection>()?;
    Ok(())
}
