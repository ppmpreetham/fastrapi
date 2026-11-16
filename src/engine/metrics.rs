//! The Prometheus metrics support of fastrapi.
//! It contains the global Prometheus handle and the `Instrumentator` Python class.

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use pyo3::prelude::*;
use std::sync::Arc;
use std::sync::Mutex;

use crate::engine::config::{FastrAPI, PrometheusConfig};
use pyo3::exceptions::PyRuntimeError;
use pyo3::exceptions::PyValueError;

static PROMETHEUS_HANDLE: Mutex<Option<Arc<PrometheusHandle>>> = Mutex::new(None);

/// Returns the handle of the global Prometheus recorder, installing it on the
/// first call. It is called once per app at startup, never per request.
pub(crate) fn prometheus_handle() -> PyResult<Arc<PrometheusHandle>> {
    let mut handle = PROMETHEUS_HANDLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(handle) = handle.as_ref() {
        return Ok(Arc::clone(handle));
    }
    let installed = Arc::new(PrometheusBuilder::new().install_recorder().map_err(|err| {
        PyRuntimeError::new_err(format!("failed to install the Prometheus recorder: {err}"))
    })?);
    *handle = Some(Arc::clone(&installed));
    drop(handle);
    Ok(installed)
}

#[pyclass(
    frozen,
    new = "from_fields",
    name = "Instrumentator",
    skip_from_py_object
)]
pub struct PyInstrumentator;

#[pymethods]
impl PyInstrumentator {
    #[pyo3(signature = (app))]
    fn instrument(slf: Py<Self>, app: Bound<'_, FastrAPI>) -> Py<Self> {
        drop(app);
        slf
    }

    #[pyo3(signature = (app, endpoint = "/metrics"))]
    fn expose(slf: Py<Self>, app: &Bound<'_, FastrAPI>, endpoint: &str) -> PyResult<Py<Self>> {
        if !endpoint.starts_with('/') {
            return Err(PyValueError::new_err(
                "Prometheus metrics endpoint must start with '/'",
            ));
        }

        let mut app_mut = app.borrow_mut();
        app_mut.prometheus_config = Some(PrometheusConfig {
            metrics_path: endpoint.to_string(),
        });
        Ok(slf)
    }
}
