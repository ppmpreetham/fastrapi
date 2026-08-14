use pyo3::prelude::*;
use pyo3::types::{PyAny, PyString};

crate::cached_py_import!(INSPECT_PARAMETER_EMPTY, "inspect", "Parameter", "empty");

pub fn is_inspect_empty(py: Python<'_>, value: &Bound<'_, PyAny>) -> bool {
    INSPECT_PARAMETER_EMPTY.is(py, value)
}

pub fn is_ellipsis(value: &Bound<'_, PyAny>) -> bool {
    value
        .get_type()
        .name()
        .map(|name| name == "ellipsis")
        .unwrap_or(false)
}

pub fn annotation_name(py: Python<'_>, annotation: &Py<PyAny>) -> Option<String> {
    let annotation = annotation.bind(py);
    annotation
        .getattr("__name__")
        .ok()
        .and_then(|value| {
            value
                .cast::<PyString>()
                .ok()
                .and_then(|name| name.to_str().ok())
                .map(str::to_owned)
        })
        .or_else(|| {
            annotation
                .str()
                .ok()
                .map(|value| value.to_string_lossy().into_owned())
        })
}

#[inline]
pub fn is_upload_file_type(name: &str) -> bool {
    name == "UploadFile" || name.ends_with(".UploadFile")
}

#[inline]
pub fn is_background_tasks_type(name: &str) -> bool {
    name == "BackgroundTasks" || name.ends_with(".BackgroundTasks")
}
