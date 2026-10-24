use pyo3::prelude::*;
use std::sync::Arc;

#[pyclass(
    name = "SecurityScopes",
    module = "fastrapi.security",
    skip_from_py_object
)]
#[derive(Clone, Debug)]
pub struct PySecurityScopes {
    pub scopes: Arc<[String]>,
}

#[pymethods]
impl PySecurityScopes {
    #[new]
    #[pyo3(signature = (scopes=None))]
    #[must_use]
    pub fn new(scopes: Option<Vec<String>>) -> Self {
        Self {
            scopes: scopes.unwrap_or_default().into(),
        }
    }

    #[getter]
    fn scopes(&self) -> Vec<String> {
        self.scopes.to_vec()
    }

    #[getter]
    fn scope_str(&self) -> String {
        self.scopes.join(" ")
    }
}
