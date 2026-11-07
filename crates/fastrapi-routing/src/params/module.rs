use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyModule, PyString};

const PARAMS_SOURCE: &str = include_str!("../py/params.py");

static PARAMS: PyOnceLock<Py<PyModule>> = PyOnceLock::new();
static ANALYZE: PyOnceLock<Py<PyAny>> = PyOnceLock::new();
static ERRORS_WITH_LOC: PyOnceLock<Py<PyAny>> = PyOnceLock::new();
static JSON_ERROR: PyOnceLock<Py<PyAny>> = PyOnceLock::new();

pub fn params_module(py: Python<'_>) -> PyResult<&Bound<'_, PyModule>> {
    PARAMS
        .get_or_try_init(py, || {
            fastrapi_http::exceptions::exceptions_module(py)?;
            let module = fastrapi_pyutil::imports::python_source_module(
                py,
                PARAMS_SOURCE,
                c"fastrapi/params.py",
                c"fastrapi.params",
            )?;
            fastrapi_pyutil::imports::register_module(py, "fastrapi.params", &module)?;
            Ok::<_, PyErr>(module.unbind())
        })
        .map(|module| module.bind(py))
}

fn helper<'py>(
    py: Python<'py>,
    cell: &'static PyOnceLock<Py<PyAny>>,
    name: &Bound<'py, PyString>,
) -> PyResult<&'py Bound<'py, PyAny>> {
    cell.get_or_try_init(py, || params_module(py)?.getattr(name).map(Bound::unbind))
        .map(|function| function.bind(py))
}

pub fn analyze(py: Python<'_>) -> PyResult<&Bound<'_, PyAny>> {
    helper(py, &ANALYZE, intern!(py, "analyze"))
}

pub fn errors_with_loc(py: Python<'_>) -> PyResult<&Bound<'_, PyAny>> {
    helper(py, &ERRORS_WITH_LOC, intern!(py, "errors_with_loc"))
}

pub fn json_error(py: Python<'_>) -> PyResult<&Bound<'_, PyAny>> {
    helper(py, &JSON_ERROR, intern!(py, "json_error"))
}
