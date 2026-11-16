//! This module contains the `StaticFiles` class.
//! The class sends files from a directory or from Python packages.
//! Some methods send the call to the starlette `StaticFiles` class.

use pyo3::exceptions::{PyAssertionError, PyRuntimeError};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyTuple};
use std::path::Path;

fastrapi_pyutil::cached_py_import!(
    STARLETTE_STATIC_FILES,
    "starlette.staticfiles",
    "StaticFiles"
);

#[pyclass(frozen, name = "StaticFiles", from_py_object, eq)]
pub struct PyStaticFiles {
    #[pyo3(get)]
    pub directory: String,
    #[pyo3(get)]
    pub directories: Vec<String>,
    #[pyo3(get)]
    pub html: bool,
    #[pyo3(get)]
    pub check_dir: bool,
    #[pyo3(get)]
    pub follow_symlink: bool,
    /// The Starlette `StaticFiles` the methods are sent to, built on first use.
    delegate: PyOnceLock<Py<PyAny>>,
}

impl Clone for PyStaticFiles {
    fn clone(&self) -> Self {
        Self {
            directory: self.directory.clone(),
            directories: self.directories.clone(),
            html: self.html,
            check_dir: self.check_dir,
            follow_symlink: self.follow_symlink,
            delegate: PyOnceLock::new(),
        }
    }
}

impl PartialEq for PyStaticFiles {
    fn eq(&self, other: &Self) -> bool {
        self.directory == other.directory
            && self.directories == other.directories
            && self.html == other.html
            && self.check_dir == other.check_dir
            && self.follow_symlink == other.follow_symlink
    }
}

impl Eq for PyStaticFiles {}

impl PyStaticFiles {
    fn delegate<'py>(&self, py: Python<'py>) -> PyResult<&Bound<'py, PyAny>> {
        self.delegate
            .get_or_try_init(py, || {
                let kwargs = PyDict::new(py);
                kwargs.set_item(pyo3::intern!(py, "directory"), &self.directory)?;
                kwargs.set_item(pyo3::intern!(py, "html"), self.html)?;
                kwargs.set_item(pyo3::intern!(py, "check_dir"), self.check_dir)?;
                kwargs.set_item(pyo3::intern!(py, "follow_symlink"), self.follow_symlink)?;
                let delegate = STARLETTE_STATIC_FILES.get(py)?.call((), Some(&kwargs))?;
                delegate.setattr(pyo3::intern!(py, "all_directories"), &self.directories)?;
                Ok(delegate.unbind())
            })
            .map(|delegate| delegate.bind(py))
    }
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
            delegate: PyOnceLock::new(),
        })
    }

    fn __call__<'py>(
        &self,
        py: Python<'py>,
        scope: &Bound<'py, PyAny>,
        receive: &Bound<'py, PyAny>,
        send: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.delegate(py)?.call1((scope, receive, send))
    }

    /// Given `directory` and `packages` arguments, return a list of all the
    /// directories that should be used for serving static files from.
    #[pyo3(signature = (directory=None, packages=None))]
    fn get_directories(
        &self,
        directory: Option<String>,
        packages: Option<Py<PyAny>>,
    ) -> PyResult<Vec<String>> {
        resolve_directories(directory, packages)
    }

    /// Given the ASGI scope, return the `path` string to serve up,
    /// with OS specific path separators, and any '..', '.' components removed.
    fn get_path(&self, py: Python<'_>, scope: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        Ok(self
            .delegate(py)?
            .call_method1(pyo3::intern!(py, "get_path"), (scope,))?
            .unbind())
    }

    fn lookup_path(&self, py: Python<'_>, path: &str) -> PyResult<Py<PyAny>> {
        Ok(self
            .delegate(py)?
            .call_method1(pyo3::intern!(py, "lookup_path"), (path,))?
            .unbind())
    }

    /// Returns an HTTP response, given the incoming path, method and request headers.
    fn get_response<'py>(
        &self,
        py: Python<'py>,
        path: &str,
        scope: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.delegate(py)?
            .call_method1(pyo3::intern!(py, "get_response"), (path, scope))
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
            Some(code) => delegate.call_method1(
                pyo3::intern!(py, "file_response"),
                (full_path, stat_result, scope, code),
            ),
            None => delegate.call_method1(
                pyo3::intern!(py, "file_response"),
                (full_path, stat_result, scope),
            ),
        }
    }

    /// Perform a one-off configuration check that StaticFiles is actually
    /// pointed at a directory, so that we can raise loud errors rather than
    /// just returning 404 responses.
    fn check_config<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.delegate(py)?
            .call_method0(pyo3::intern!(py, "check_config"))
    }

    /// Given the request and response headers, return `True` if an HTTP
    /// "Not Modified" response could be returned instead.
    fn is_not_modified<'py>(
        &self,
        py: Python<'py>,
        response_headers: &Bound<'py, PyAny>,
        request_headers: &Bound<'py, PyAny>,
    ) -> PyResult<bool> {
        self.delegate(py)?
            .call_method1(
                pyo3::intern!(py, "is_not_modified"),
                (response_headers, request_headers),
            )?
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
            let (package, statics_dir) = match item.cast::<PyTuple>() {
                Ok(pair) => (
                    pair.get_item(0)?.extract::<String>()?,
                    pair.get_item(1)?.extract::<String>()?,
                ),
                Err(_) => (item.extract::<String>()?, "statics".to_owned()),
            };
            let origin = py
                .import("importlib.util")?
                .call_method1(pyo3::intern!(py, "find_spec"), (&package,))?
                .getattr(pyo3::intern!(py, "origin"))
                .ok()
                .filter(|origin| !origin.is_none())
                .ok_or_else(|| {
                    PyAssertionError::new_err(format!("Package '{package}' could not be found."))
                })?;
            let os_path = py.import("os.path")?;
            let joined =
                os_path.call_method1(pyo3::intern!(py, "join"), (origin, "..", &statics_dir))?;
            let package_directory: String = os_path
                .call_method1(pyo3::intern!(py, "normpath"), (joined,))?
                .extract()?;
            if !Path::new(&package_directory).is_dir() {
                return Err(PyAssertionError::new_err(format!(
                    "Directory ''{statics_dir}'' in package '{package}' could not be found."
                )));
            }
            directories.push(package_directory);
        }
        Ok(directories)
    })
}
