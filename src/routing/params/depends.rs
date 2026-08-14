use pyo3::prelude::*;

#[pyclass(name = "Depends", subclass, get_all, from_py_object)]
#[derive(Clone)]
pub struct PyDepends {
    pub dependency: Option<Py<PyAny>>,
    pub use_cache: bool,
}

#[pymethods]
impl PyDepends {
    #[new]
    #[pyo3(signature = (dependency=None, *, use_cache=true))]
    pub fn new(dependency: Option<Py<PyAny>>, use_cache: bool) -> Self {
        Self {
            dependency,
            use_cache,
        }
    }
}

#[pyclass(name = "Security", extends = PyDepends, get_all, from_py_object)]
#[derive(Clone)]
pub struct PySecurity {
    pub scopes: Vec<String>,
}

#[pymethods]
impl PySecurity {
    #[new]
    #[pyo3(signature = (dependency=None, *, scopes=None, use_cache=true))]
    fn new(
        dependency: Option<Py<PyAny>>,
        scopes: Option<Vec<String>>,
        use_cache: bool,
    ) -> pyo3::PyClassInitializer<Self> {
        pyo3::PyClassInitializer::from(PyDepends::new(dependency, use_cache)).add_subclass(Self {
            scopes: scopes.unwrap_or_default(),
        })
    }
}
