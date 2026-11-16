//! Public classes that extend a Rust class and a Python base class, such as one
//! of Starlette's, so that `isinstance` checks against the base pass like in FastAPI.

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyType};
use pyo3::PyTypeInfo;

/// Builds the public class once. The Rust constructor creates the instances,
/// and the Python base's `__init__` never runs.
pub struct NativeSubclass {
    base_module: &'static str,
    base_name: &'static str,
    module: &'static str,
    class: PyOnceLock<Py<PyType>>,
}

impl NativeSubclass {
    #[must_use]
    pub const fn new(base_module: &'static str, base_name: &'static str, module: &'static str) -> Self {
        Self {
            base_module,
            base_name,
            module,
            class: PyOnceLock::new(),
        }
    }

    /// Returns the public class over the Rust class `T`.
    pub fn get<'py, T: PyTypeInfo>(&self, py: Python<'py>) -> PyResult<&Bound<'py, PyType>> {
        self.class
            .get_or_try_init(py, || {
                let native = T::type_object(py);
                let base = py.import(self.base_module)?.getattr(self.base_name)?;
                let namespace = PyDict::new(py);
                namespace.set_item(intern!(py, "__module__"), self.module)?;
                namespace.set_item(intern!(py, "__doc__"), native.getattr(intern!(py, "__doc__"))?)?;
                namespace.set_item(
                    intern!(py, "__init__"),
                    PyAny::type_object(py).getattr(intern!(py, "__init__"))?,
                )?;
                let class = PyType::type_object(py).call1((native.name()?, (native, base), namespace))?;
                Ok::<_, PyErr>(class.cast_into::<PyType>()?.unbind())
            })
            .map(|class| class.bind(py))
    }
}
