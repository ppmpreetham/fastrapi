//! Cached Python modules and attributes that many crates use.
//! Each item is imported one time, on first use.

use crate::cached_py_import;
use pyo3::prelude::*;
use pyo3::types::PyModule;

cached_py_import!(pub INSPECT, "inspect");
cached_py_import!(pub TYPING, "typing");
cached_py_import!(pub ENUM, "enum");
cached_py_import!(pub TYPES, "types");
cached_py_import!(pub STARLETTE_STATE, "starlette.datastructures", "State");
cached_py_import!(pub STARLETTE_HEADERS, "starlette.datastructures", "Headers");
cached_py_import!(pub SIMPLE_COOKIE, "http.cookies", "SimpleCookie");
cached_py_import!(SYS_MODULES, "sys", "modules");

pub fn python_source_module<'py>(
    py: Python<'py>,
    source: &str,
    file_name: &std::ffi::CStr,
    module_name: &std::ffi::CStr,
) -> PyResult<Bound<'py, PyModule>> {
    let source = std::ffi::CString::new(source)
        .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
    PyModule::from_code(py, &source, file_name, module_name)
}

pub fn register_module(py: Python<'_>, name: &str, module: &Bound<'_, PyModule>) -> PyResult<()> {
    SYS_MODULES.get(py)?.set_item(name, module)
}
