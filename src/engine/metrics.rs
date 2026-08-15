use axum::{Router, routing::get};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use pyo3::prelude::*;
use std::sync::{Arc, OnceLock};

use crate::engine::types::{FastrAPI, PrometheusConfig};

static PROMETHEUS_HANDLE: OnceLock<Arc<PrometheusHandle>> = OnceLock::new();

pub fn prometheus_handle() -> Arc<PrometheusHandle> {
    PROMETHEUS_HANDLE
        .get_or_init(|| {
            Arc::new(
                PrometheusBuilder::new()
                    .install_recorder()
                    .expect("failed to install Prometheus recorder"),
            )
        })
        .clone()
}

pub fn init_prometheus(path: &str) -> Router {
    let prometheus_handle = prometheus_handle();

    Router::new().route(
        path,
        get(move || {
            let prometheus_handle = prometheus_handle.clone();
            async move { prometheus_handle.render() }
        }),
    )
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
    fn expose(slf: Py<Self>, app: Bound<'_, FastrAPI>, endpoint: &str) -> PyResult<Py<Self>> {
        if !endpoint.starts_with('/') {
            return Err(pyo3::exceptions::PyValueError::new_err(
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
