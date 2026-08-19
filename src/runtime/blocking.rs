use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use crossbeam_channel as channel;
use pyo3::Python;

const IDLE_TIMEOUT_SECS: u64 = 30;

pub(crate) struct BlockingTask {
    inner: Box<dyn FnOnce(Python<'_>) + Send + 'static>,
}

impl BlockingTask {
    #[inline]
    pub fn new<T>(inner: T) -> Self
    where
        T: FnOnce(Python<'_>) + Send + 'static,
    {
        Self {
            inner: Box::new(inner),
        }
    }

    #[inline(always)]
    fn run(self, py: Python<'_>) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (self.inner)(py);
        }));
    }
}

pub struct BlockingRunner {
    queue: channel::Sender<BlockingTask>,
    tq: channel::Receiver<BlockingTask>,
    threads: Arc<AtomicUsize>,
    tmax: usize,
    idle: Arc<AtomicUsize>,
    idle_timeout: Duration,
}

impl BlockingRunner {
    fn new(max_threads: usize) -> Self {
        let (qtx, qrx) = channel::unbounded();
        let idle = Arc::new(AtomicUsize::new(0));
        let runner = Self {
            queue: qtx,
            tq: qrx.clone(),
            threads: Arc::new(AtomicUsize::new(1)),
            tmax: max_threads.max(1),
            idle,
            idle_timeout: Duration::from_secs(IDLE_TIMEOUT_SECS),
        };

        let idle_init = runner.idle.clone();
        thread::Builder::new()
            .name("fastrapi-py-blocking".into())
            .spawn(move || blocking_worker_idle(qrx, idle_init))
            .expect("failed to spawn blocking runner thread");

        runner
    }

    #[inline]
    fn spawn_thread(&self, current_count: usize) {
        if self
            .threads
            .compare_exchange(
                current_count,
                current_count + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_err()
        {
            return;
        }

        let queue = self.tq.clone();
        let threads = self.threads.clone();
        let idle = self.idle.clone();
        let timeout = self.idle_timeout;

        let _ = thread::Builder::new()
            .name("fastrapi-py-blocking".into())
            .spawn(move || {
                blocking_worker_timeout(queue, timeout, idle);
                threads.fetch_sub(1, Ordering::Relaxed);
            });
    }

    #[inline]
    pub fn run<T>(&self, task: T)
    where
        T: FnOnce(Python<'_>) + Send + 'static,
    {
        if self.queue.send(BlockingTask::new(task)).is_err() {
            return;
        }

        let threads = self.threads.load(Ordering::Relaxed);
        let idle = self.idle.load(Ordering::Relaxed);
        if self.queue.len() > idle && threads < self.tmax {
            self.spawn_thread(threads);
        }
    }
}

fn blocking_worker_idle(queue: channel::Receiver<BlockingTask>, idle: Arc<AtomicUsize>) {
    Python::attach(|py| {
        while let Ok(task) = py.detach(|| {
            idle.fetch_add(1, Ordering::Relaxed);
            let task = queue.recv();
            idle.fetch_sub(1, Ordering::Relaxed);
            task
        }) {
            task.run(py);
        }
    });
}

fn blocking_worker_timeout(
    queue: channel::Receiver<BlockingTask>,
    timeout: Duration,
    idle: Arc<AtomicUsize>,
) {
    Python::attach(|py| {
        while let Ok(task) = py.detach(|| {
            idle.fetch_add(1, Ordering::Relaxed);
            let task = queue.recv_timeout(timeout);
            idle.fetch_sub(1, Ordering::Relaxed);
            task
        }) {
            task.run(py);
        }
    });
}

static RUNNER: OnceLock<BlockingRunner> = OnceLock::new();

#[inline]
fn runner() -> &'static BlockingRunner {
    RUNNER.get_or_init(|| {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        BlockingRunner::new(crate::globals::config().sync_threads.max(cpus))
    })
}

#[inline]
pub fn dispatch<T>(task: T)
where
    T: FnOnce(Python<'_>) + Send + 'static,
{
    runner().run(task);
}

#[inline]
pub fn run_python<T, F>(task: F) -> tokio::sync::oneshot::Receiver<T>
where
    T: Send + 'static,
    F: FnOnce(Python<'_>) -> T + Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    dispatch(move |py| {
        let _ = tx.send(task(py));
    });
    rx
}
