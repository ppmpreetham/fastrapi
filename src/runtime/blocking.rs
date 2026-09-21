use pyo3::Python;

#[inline]
pub async fn run_python<T, F>(task: F) -> Result<T, tokio::task::JoinError>
where
    T: Send + 'static,
    F: FnOnce(Python<'_>) -> T + Send + 'static,
{
    tokio::task::spawn_blocking(move || Python::attach(task)).await
}
