use parking_lot::RwLock;
use std::future::Future;
use std::sync::LazyLock;
use std::sync::OnceLock;
crate::cached_py_import!(pub BASEMODEL_TYPE, "pydantic", "BaseModel");

pub static PYTHON_RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(cpus)
        .thread_name("python-handler")
        .enable_all()
        .build()
        .expect("Failed to create Python runtime")
});

#[inline]
pub fn spawn<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    PYTHON_RUNTIME.spawn(future)
}

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
    static CONFIG: LazyLock<Config> = LazyLock::new(Config::default);
    &CONFIG
}

static DEPENDENCY_OVERRIDES: RwLock<Option<ahash::AHashMap<u64, pyo3::Py<pyo3::PyAny>>>> =
    RwLock::new(None);

pub(crate) fn set_dependency_overrides(map: ahash::AHashMap<u64, pyo3::Py<pyo3::PyAny>>) {
    *DEPENDENCY_OVERRIDES.write() = Some(map);
}

#[allow(clippy::implicit_hasher)]
pub(crate) fn take_dependency_overrides() -> ahash::AHashMap<u64, pyo3::Py<pyo3::PyAny>> {
    DEPENDENCY_OVERRIDES.read().clone().unwrap_or_default()
}

static ASYNC_LOOP: OnceLock<pyo3::Py<pyo3::PyAny>> = OnceLock::new();

pub(crate) fn set_async_loop(loop_: pyo3::Py<pyo3::PyAny>) {
    _ = ASYNC_LOOP.set(loop_);
}

pub(crate) fn async_loop() -> Option<&'static pyo3::Py<pyo3::PyAny>> {
    ASYNC_LOOP.get()
}

static SERVE_APP: OnceLock<pyo3::Py<pyo3::PyAny>> = OnceLock::new();

pub fn set_serve_app(app: pyo3::Py<pyo3::PyAny>) {
    _ = SERVE_APP.set(app);
}

pub fn serve_app() -> Option<&'static pyo3::Py<pyo3::PyAny>> {
    SERVE_APP.get()
}

static EXCEPTION_HANDLERS: RwLock<Option<pyo3::Py<pyo3::types::PyDict>>> = RwLock::new(None);

pub(crate) fn set_exception_handlers(registry: pyo3::Py<pyo3::types::PyDict>) {
    *EXCEPTION_HANDLERS.write() = Some(registry);
}

pub(crate) fn exception_handlers() -> Option<pyo3::Py<pyo3::types::PyDict>> {
    EXCEPTION_HANDLERS.read().clone()
}
