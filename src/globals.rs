use once_cell::sync::Lazy;
use papaya::HashMap as PapayaHashMap;
use pyo3::prelude::*;
use pyo3::types::PyType;
use crate::http::middleware::PyMiddleware;
use crate::routing::types::RouteHandler;
use std::sync::{Arc, OnceLock};

pub static WEBSOCKET_ROUTES: Lazy<PapayaHashMap<String, Py<PyAny>>> =
    Lazy::new(|| PapayaHashMap::with_capacity(32));

pub static ROUTES: Lazy<PapayaHashMap<String, Arc<RouteHandler>>> =
    Lazy::new(|| PapayaHashMap::with_capacity(128));

pub static MIDDLEWARES: Lazy<PapayaHashMap<String, Arc<PyMiddleware>>> =
    Lazy::new(|| PapayaHashMap::with_capacity(16));

pub static BASEMODEL_TYPE: OnceLock<Py<PyType>> = OnceLock::new();

pub static PYTHON_RUNTIME: Lazy<tokio::runtime::Runtime> = Lazy::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(num_cpus::get().max(4).min(16))
        .thread_name("python-handler")
        .enable_all()
        .build()
        .expect("Failed to create Python runtime")
});

// Config
#[derive(Debug, Default)]
pub struct Config {
    // TODO: fields later
}

pub fn config() -> &'static Config {
    static CONFIG: std::sync::LazyLock<Config> = std::sync::LazyLock::new(Config::default);
    &*CONFIG
}
