//! Runs blocking Python code on a dedicated thread pool.
//! Each pool thread keeps one Python thread state while jobs keep coming and releases it only when
//! the queue is empty, so a busy pool never creates or destroys thread states.

use crate::thread_state;
use parking_lot::{Condvar, Mutex};
use pyo3::Python;
use std::any::Any;
use std::collections::VecDeque;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::LazyLock;
use tokio::sync::oneshot;

type Job = Box<dyn FnOnce() + Send>;

struct Queue {
    jobs: VecDeque<Job>,
    idle: usize,
    threads: usize,
}

struct Pool {
    queue: Mutex<Queue>,
    ready: Condvar,
    max_threads: usize,
}

static POOL: LazyLock<Pool> = LazyLock::new(|| Pool {
    queue: Mutex::new(Queue {
        jobs: VecDeque::new(),
        idle: 0,
        threads: 0,
    }),
    ready: Condvar::new(),
    max_threads: crate::tokio_runtime::blocking_threads().max(1),
});

impl Pool {
    fn submit(&'static self, job: Job) {
        let mut queue = self.queue.lock();
        queue.jobs.push_back(job);
        if queue.idle > 0 {
            self.ready.notify_one();
        }
        if queue.jobs.len() > queue.idle && queue.threads < self.max_threads {
            queue.threads += 1;
            drop(queue);
            let spawned = std::thread::Builder::new()
                .name("python-blocking".into())
                .spawn(move || self.run());
            if spawned.is_err() {
                self.queue.lock().threads -= 1;
            }
        }
    }

    fn run(&self) {
        thread_state::mark_worker();
        loop {
            let job = {
                let mut queue = self.queue.lock();
                loop {
                    if let Some(job) = queue.jobs.pop_front() {
                        break job;
                    }
                    if thread_state::is_held() {
                        drop(queue);
                        thread_state::release();
                        queue = self.queue.lock();
                        continue;
                    }
                    queue.idle += 1;
                    self.ready.wait(&mut queue);
                    queue.idle -= 1;
                }
            };
            thread_state::hold();
            job();
        }
    }
}

/// The blocking task panicked.
pub struct Panicked(Box<dyn Any + Send>);

impl fmt::Debug for Panicked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Panicked")
    }
}

impl fmt::Display for Panicked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = self
            .0
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| self.0.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("blocking task panicked");
        f.write_str(message)
    }
}

impl std::error::Error for Panicked {}

#[inline]
pub async fn run_python<T, F>(task: F) -> Result<T, Panicked>
where
    T: Send + 'static,
    F: FnOnce(Python<'_>) -> T + Send + 'static,
{
    let (tx, rx) = oneshot::channel();
    POOL.submit(Box::new(move || {
        let result = catch_unwind(AssertUnwindSafe(|| Python::attach(task)));
        _ = tx.send(result.map_err(Panicked));
    }));
    rx.await.expect("blocking pool dropped a job")
}
