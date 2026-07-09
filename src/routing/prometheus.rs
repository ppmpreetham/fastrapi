use axum::{Router, routing::get};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use std::sync::Arc;

fn setup_prometheus() -> Arc<PrometheusHandle> {
    let handle = PrometheusBuilder::new()
        .install_recorder()
        .expect("Failed to install Prometheus recorder");

    Arc::new(handle)
}

fn init_prometheus() -> Router {
    let prometheus_handle = setup_prometheus();

    Router::new().route(
        "/metrics",
        get(move || async move { prometheus_handle.render() }),
    )
}

// TODO: in the main router function, merge this router with the main router to expose the /metrics endpoint
// PrometheusInstrumentator().instrument(app).expose(app)
