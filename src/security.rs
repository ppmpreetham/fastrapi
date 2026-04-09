use pyo3::prelude::*;
use pyo3_nest::add_classes;

#[pyclass(
    name = "SecurityScopes",
    module = "fastrapi.security",
    skip_from_py_object
)]
#[derive(Clone, Debug)]
pub struct PySecurityScopes {
    #[pyo3(get)]
    pub scopes: Vec<String>,
}

#[pymethods]
impl PySecurityScopes {
    #[new]
    #[pyo3(signature = (scopes=None))]
    pub fn new(scopes: Option<Vec<String>>) -> Self {
        Self {
            scopes: scopes.unwrap_or_default(),
        }
    }

    #[getter]
    fn scope_str(&self) -> String {
        self.scopes.join(" ")
    }
}
