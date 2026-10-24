//! The tokio runtime that runs the Python handlers.
//! The runtime has one worker thread for each CPU and a configurable number of blocking threads.

use std::future::Future;
use std::num::NonZero;
use std::sync::LazyLock;
use std::thread::available_parallelism;
use tokio::runtime::{Builder, Runtime};
use tokio::task::JoinHandle;

pub static PYTHON_RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    let cpus = available_parallelism().map_or(4, NonZero::get);

    Builder::new_multi_thread()
        .worker_threads(cpus)
        .max_blocking_threads(config().sync_threads.max(cpus))
        .thread_name("python-handler")
        .enable_all()
        .build()
        .expect("Failed to create Python runtime")
});

#[inline]
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
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
        let cpus = available_parallelism().map_or(4, NonZero::get);
        Self {
            sync_threads: cpus * 4,
        }
    }
}

#[must_use]
pub fn config() -> &'static Config {
    static CONFIG: LazyLock<Config> = LazyLock::new(Config::default);
    &CONFIG
}
