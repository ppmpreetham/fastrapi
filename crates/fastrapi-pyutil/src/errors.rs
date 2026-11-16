//! Helpers for passing a Python exception back into Python code.

use pyo3::exceptions::PyBaseException;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyType};

/// Returns the `(type, value, traceback)` triple that `throw`, `athrow` and `__aexit__` take.
#[must_use]
pub fn exc_info<'py>(
    py: Python<'py>,
    err: &PyErr,
) -> (
    Bound<'py, PyType>,
    Bound<'py, PyBaseException>,
    Bound<'py, PyAny>,
) {
    let traceback = err
        .traceback(py)
        .map_or_else(|| py.None().into_bound(py), Bound::into_any);
    (err.get_type(py), err.value(py).clone(), traceback)
}
