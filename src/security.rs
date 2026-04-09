use pyo3::prelude::*;
use pyo3_nest::add_classes;

#[pyclass(name = "SecurityScopes", module = "fastrapi.security")]
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

pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    add_classes!(parent, PySecurityScopes);
    Ok(())
}
