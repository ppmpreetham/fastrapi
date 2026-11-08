//! The tokio runtime that runs the Python handlers.

use std::future::Future;
use std::num::NonZero;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, OnceLock};
use std::thread::available_parallelism;
use tokio::runtime::{Builder, Runtime};
use tokio::task::JoinHandle;

static REQUESTED_THREADS: OnceLock<usize> = OnceLock::new();
static RUNTIME_BUILT: AtomicBool = AtomicBool::new(false);

pub fn set_threads(threads: NonZero<usize>) -> Result<(), &'static str> {
    if RUNTIME_BUILT.load(Ordering::Acquire) {
        return Err("threads must be set before the server runtime starts");
    }
    let stored = *REQUESTED_THREADS.get_or_init(|| threads.get());
    if stored == threads.get() {
        Ok(())
    } else {
        Err("threads was already set to a different value")
    }
}

fn requested_threads() -> Option<usize> {
    REQUESTED_THREADS.get().copied().or_else(|| {
        std::env::var("FASTRAPI_WORKER_THREADS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
    })
}

fn thread_counts() -> (usize, usize) {
    if let Some(n) = requested_threads() {
        (n, n)
    } else {
        let cpus = available_parallelism().map_or(4, NonZero::get);
        (cpus, config().sync_threads.max(cpus))
    }
}

pub(crate) fn blocking_threads() -> usize {
    RUNTIME_BUILT.store(true, Ordering::Release);
    thread_counts().1
}

pub static PYTHON_RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    RUNTIME_BUILT.store(true, Ordering::Release);
    let (workers, blocking) = thread_counts();

    Builder::new_multi_thread()
        .worker_threads(workers)
        .max_blocking_threads(blocking)
        .thread_name("python-handler")
        .on_thread_unpark(crate::thread_state::mark_worker)
        .on_thread_park(crate::thread_state::flush)
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

#[derive(Debug)]
pub struct Config {
    pub sync_threads: usize,
}

impl Default for Config {
    fn default() -> Self {
        let sync_threads = requested_threads()
            .unwrap_or_else(|| available_parallelism().map_or(4, NonZero::get) * 4);
        Self { sync_threads }
    }
}

#[must_use]
pub fn config() -> &'static Config {
    static CONFIG: LazyLock<Config> = LazyLock::new(Config::default);
    &CONFIG
}
