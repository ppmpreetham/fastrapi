use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::PyModule;

pub struct LazyPyModule {
    module_name: &'static str,
    cell: PyOnceLock<Py<PyModule>>,
}

impl LazyPyModule {
    pub const fn new(module_name: &'static str) -> Self {
        Self {
            module_name,
            cell: PyOnceLock::new(),
        }
    }

    #[inline]
    pub fn get<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyModule>> {
        let module_ref = self
            .cell
            .get_or_try_init(py, || py.import(self.module_name).map(|m| m.unbind()))?;
        Ok(module_ref.bind(py).clone())
    }
}

pub struct LazyPyAttr {
    module_name: &'static str,
    attr_name: &'static str,
    cell: PyOnceLock<Py<PyAny>>,
}

impl LazyPyAttr {
    pub const fn new(module_name: &'static str, attr_name: &'static str) -> Self {
        Self {
            module_name,
            attr_name,
            cell: PyOnceLock::new(),
        }
    }

    #[inline]
    pub fn get<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let attr_ref = self.cell.get_or_try_init(py, || {
            let module = py.import(self.module_name)?;
            module.getattr(self.attr_name).map(|a| a.unbind())
        })?;
        Ok(attr_ref.bind(py).clone())
    }

    #[inline]
    pub fn is_instance(&self, py: Python<'_>, value: &Bound<'_, PyAny>) -> bool {
        self.get(py)
            .and_then(|cls| value.is_instance(&cls))
            .unwrap_or(false)
    }

    #[inline]
    pub fn is(&self, py: Python<'_>, value: &Bound<'_, PyAny>) -> bool {
        self.get(py)
            .map(|target| value.is(&target))
            .unwrap_or(false)
    }
}

pub struct LazyPyNestedAttr {
    module_name: &'static str,
    parent_attr: &'static str,
    child_attr: &'static str,
    cell: PyOnceLock<Py<PyAny>>,
}

impl LazyPyNestedAttr {
    pub const fn new(
        module_name: &'static str,
        parent_attr: &'static str,
        child_attr: &'static str,
    ) -> Self {
        Self {
            module_name,
            parent_attr,
            child_attr,
            cell: PyOnceLock::new(),
        }
    }

    #[inline]
    pub fn get<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let attr_ref = self.cell.get_or_try_init(py, || {
            let module = py.import(self.module_name)?;
            let parent = module.getattr(self.parent_attr)?;
            parent.getattr(self.child_attr).map(|a| a.unbind())
        })?;
        Ok(attr_ref.bind(py).clone())
    }

    #[inline]
    pub fn is(&self, py: Python<'_>, value: &Bound<'_, PyAny>) -> bool {
        self.get(py)
            .map(|target| value.is(&target))
            .unwrap_or(false)
    }
}
