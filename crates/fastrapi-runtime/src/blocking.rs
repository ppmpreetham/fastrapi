use pyo3::Python;
use tokio::task::{JoinError, spawn_blocking};

#[inline]
pub async fn run_python<T, F>(task: F) -> Result<T, JoinError>
where
    T: Send + 'static,
    F: FnOnce(Python<'_>) -> T + Send + 'static,
{
    spawn_blocking(move || Python::attach(task)).await
}
