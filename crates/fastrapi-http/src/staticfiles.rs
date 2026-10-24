use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};
use std::path::{Path, PathBuf};

#[pyclass(frozen, name = "StaticFiles", get_all, from_py_object, eq)]
#[derive(Clone, PartialEq, Eq)]
pub struct PyStaticFiles {
    pub directory: String,
    pub directories: Vec<String>,
    pub html: bool,
    pub check_dir: bool,
    pub follow_symlink: bool,
}

#[pymethods]
impl PyStaticFiles {
    #[new]
    #[pyo3(signature = (directory=None, *, packages=None, html=false, check_dir=true, follow_symlink=false))]
    fn new(
        directory: Option<String>,
        packages: Option<Py<PyAny>>,
        html: bool,
        check_dir: bool,
        follow_symlink: bool,
    ) -> PyResult<Self> {
        let directories = resolve_directories(directory, packages)?;
        let directory = directories.first().cloned().ok_or_else(|| {
            PyRuntimeError::new_err("StaticFiles requires a directory or packages")
        })?;

        if check_dir && !Path::new(&directory).is_dir() {
            return Err(PyRuntimeError::new_err(format!(
                "Directory '{directory}' does not exist"
            )));
        }

        Ok(Self {
            directory,
            directories,
            html,
            check_dir,
            follow_symlink,
        })
    }

    fn delegate<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let cls = py.import("starlette.staticfiles")?.getattr("StaticFiles")?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("directory", &self.directory)?;
        kwargs.set_item("html", self.html)?;
        kwargs.set_item("check_dir", self.check_dir)?;
        kwargs.set_item("follow_symlink", self.follow_symlink)?;
        cls.call((), Some(&kwargs))
    }

    fn get_directories(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        Ok(self
            .directories
            .clone()
            .into_pyobject(py)?
            .into_any()
            .unbind())
    }

    fn get_path(&self, py: Python<'_>, scope: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        Ok(self
            .delegate(py)?
            .call_method1("get_path", (scope,))?
            .unbind())
    }

    fn lookup_path(&self, py: Python<'_>, path: &str) -> PyResult<Py<PyAny>> {
        Ok(self
            .delegate(py)?
            .call_method1("lookup_path", (path,))?
            .unbind())
    }

    fn get_response<'py>(
        &self,
        py: Python<'py>,
        path: &str,
        scope: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.delegate(py)?
            .call_method1("get_response", (path, scope))
    }

    fn file_response<'py>(
        &self,
        py: Python<'py>,
        full_path: &Bound<'py, PyAny>,
        stat_result: &Bound<'py, PyAny>,
        scope: &Bound<'py, PyAny>,
        status_code: Option<u16>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let delegate = self.delegate(py)?;
        match status_code {
            Some(code) => {
                delegate.call_method1("file_response", (full_path, stat_result, scope, code))
            }
            None => delegate.call_method1("file_response", (full_path, stat_result, scope)),
        }
    }

    fn check_config<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.delegate(py)?.call_method0("check_config")
    }

    fn is_not_modified<'py>(
        &self,
        py: Python<'py>,
        response_headers: &Bound<'py, PyAny>,
        request_headers: &Bound<'py, PyAny>,
    ) -> PyResult<bool> {
        self.delegate(py)?
            .call_method1("is_not_modified", (response_headers, request_headers))?
            .extract()
    }
}

fn resolve_directories(
    directory: Option<String>,
    packages: Option<Py<PyAny>>,
) -> PyResult<Vec<String>> {
    let mut directories = directory.into_iter().collect::<Vec<_>>();
    let Some(packages) = packages else {
        return Ok(directories);
    };
    Python::attach(|py| {
        for item in packages.bind(py).try_iter()? {
            let item = item?;
            let (package, subdirectory) = if let Ok(name) = item.extract::<String>() {
                (name, None)
            } else {
                let pair = item.cast::<PyTuple>()?;
                (
                    pair.get_item(0)?.extract::<String>()?,
                    Some(pair.get_item(1)?.extract::<String>()?),
                )
            };
            let spec = py
                .import("importlib.util")?
                .call_method1("find_spec", (&package,))?;
            let locations = spec.getattr("submodule_search_locations")?;
            if locations.is_none() {
                return Err(PyRuntimeError::new_err(format!(
                    "Package '{package}' has no filesystem location"
                )));
            }
            for location in locations.try_iter()? {
                let mut path = PathBuf::from(location?.extract::<String>()?);
                if let Some(subdirectory) = &subdirectory {
                    path.push(subdirectory);
                }
                directories.push(path.to_string_lossy().into_owned());
            }
        }
        Ok(directories)
    })
}
