use crate::http::middleware::PyMiddleware;
use papaya::HashMap as PapayaHashMap;
use pyo3::prelude::*;
use pyo3::types::PyType;
use std::sync::{Arc, LazyLock, OnceLock, atomic::AtomicUsize};

pub static MIDDLEWARES: LazyLock<PapayaHashMap<String, Arc<PyMiddleware>>> =
    LazyLock::new(|| PapayaHashMap::with_capacity(16));

pub static MIDDLEWARE_COUNTER: AtomicUsize = AtomicUsize::new(0);

pub static BASEMODEL_TYPE: OnceLock<Py<PyType>> = OnceLock::new();

pub static PYTHON_RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(cpus.clamp(4, 16))
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
    &CONFIG
}
