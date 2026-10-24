use pyo3::prelude::*;

#[pyclass(frozen, new = "from_fields", name = "Unset", skip_from_py_object)]
#[derive(Clone)]
pub struct Unset;

#[pyclass(frozen, new = "from_fields", name = "Undefined", skip_from_py_object)]
#[derive(Clone)]
pub struct Undefined;
