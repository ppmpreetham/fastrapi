use pyo3::prelude::*;

use crate::utils::{json_to_py_object, py_any_to_json};

#[pyfunction]
fn jsonable_encoder(py: Python<'_>, obj: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    Ok(json_to_py_object(py, &py_any_to_json(py, obj)))
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(jsonable_encoder, module)?)
}
