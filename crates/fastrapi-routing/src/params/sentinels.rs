//! The sentinel classes `Unset` and `Undefined`.
//! A sentinel is a special value that shows that no value was given.

use pyo3::prelude::*;

#[pyclass(frozen, new = "from_fields", name = "Unset", skip_from_py_object)]
#[derive(Clone)]
pub struct Unset;

#[pyclass(frozen, new = "from_fields", name = "Undefined", skip_from_py_object)]
#[derive(Clone)]
pub struct Undefined;
