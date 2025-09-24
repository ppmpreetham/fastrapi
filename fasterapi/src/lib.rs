use pyo3::prelude::*;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;

use hyper::service::service_fn;

// Lazy initialization of Mutex of Hashmap : /about -> function
static ROUTES: Lazy<Mutex<HashMap<String, Py<PyAny>>>> = Lazy::new(|| Mutex::new(HashMap::new()));

#[pyfunction]
fn add(left: u64, right: u64) -> u64 {
    left + right
}

#[pymodule]
fn fasterapi(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(add, m)?)?;
    Ok(())
}