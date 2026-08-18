use std::sync::LazyLock;
crate::cached_py_import!(pub BASEMODEL_TYPE, "pydantic", "BaseModel");
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
#[derive(Debug)]
pub struct Config {
    pub sync_threads: usize,
}

impl Default for Config {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        Self {
            sync_threads: cpus * 4,
        }
    }
}

pub fn config() -> &'static Config {
    static CONFIG: std::sync::LazyLock<Config> = std::sync::LazyLock::new(Config::default);
    &CONFIG
}

static DEPENDENCY_OVERRIDES: std::sync::RwLock<
    Option<ahash::AHashMap<u64, pyo3::Py<pyo3::PyAny>>>,
> = std::sync::RwLock::new(None);

pub(crate) fn set_dependency_overrides(map: ahash::AHashMap<u64, pyo3::Py<pyo3::PyAny>>) {
    *DEPENDENCY_OVERRIDES
        .write()
        .expect("overrides lock poisoned") = Some(map);
}

#[allow(clippy::implicit_hasher)]
pub(crate) fn take_dependency_overrides() -> ahash::AHashMap<u64, pyo3::Py<pyo3::PyAny>> {
    DEPENDENCY_OVERRIDES
        .read()
        .expect("overrides lock poisoned")
        .clone()
        .unwrap_or_default()
}

static EXCEPTION_HANDLERS: std::sync::RwLock<Option<pyo3::Py<pyo3::types::PyDict>>> =
    std::sync::RwLock::new(None);

pub(crate) fn set_exception_handlers(registry: pyo3::Py<pyo3::types::PyDict>) {
    *EXCEPTION_HANDLERS.write().expect("handlers lock poisoned") = Some(registry);
}

pub(crate) fn exception_handlers() -> Option<pyo3::Py<pyo3::types::PyDict>> {
    EXCEPTION_HANDLERS
        .read()
        .expect("handlers lock poisoned")
        .clone()
}
