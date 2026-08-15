use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

#[pyclass(frozen, name = "StaticFiles", get_all, from_py_object, eq)]
#[derive(Clone, PartialEq, Eq)]
pub struct PyStaticFiles {
    pub directory: String,
    pub html: bool,
    pub check_dir: bool,
    pub follow_symlink: bool,
}

#[pymethods]
impl PyStaticFiles {
    #[new]
    #[pyo3(signature = (directory, *, packages=None, html=false, check_dir=true, follow_symlink=false))]
    fn new(
        directory: Option<String>,
        packages: Option<Py<PyAny>>,
        html: bool,
        check_dir: bool,
        follow_symlink: bool,
    ) -> PyResult<Self> {
        if packages.is_some() {
            return Err(PyRuntimeError::new_err(
                "StaticFiles(packages=...) is not supported yet",
            ));
        }

        let Some(directory) = directory else {
            return Err(PyRuntimeError::new_err("StaticFiles requires a directory"));
        };

        if check_dir && !std::path::Path::new(&directory).is_dir() {
            return Err(PyRuntimeError::new_err(format!(
                "Directory '{}' does not exist",
                directory
            )));
        }

        Ok(Self {
            directory,
            html,
            check_dir,
            follow_symlink,
        })
    }
}
