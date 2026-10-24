use pyo3::Bound;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::PyModule;

pub struct LazyPyModule {
    module_name: &'static str,
    cell: PyOnceLock<Py<PyModule>>,
}

impl LazyPyModule {
    #[must_use]
    pub const fn new(module_name: &'static str) -> Self {
        Self {
            module_name,
            cell: PyOnceLock::new(),
        }
    }

    #[inline]
    pub fn get<'a, 'py>(&'a self, py: Python<'py>) -> PyResult<&'a Bound<'py, PyModule>> {
        let module_ref = self
            .cell
            .get_or_try_init(py, || py.import(self.module_name).map(Bound::unbind))?;
        Ok(module_ref.bind(py))
    }
}

pub struct LazyPyAttr {
    module_name: &'static str,
    attr_name: &'static str,
    cell: PyOnceLock<Py<PyAny>>,
}

impl LazyPyAttr {
    #[must_use]
    pub const fn new(module_name: &'static str, attr_name: &'static str) -> Self {
        Self {
            module_name,
            attr_name,
            cell: PyOnceLock::new(),
        }
    }

    #[inline]
    pub fn get<'a, 'py>(&'a self, py: Python<'py>) -> PyResult<&'a Bound<'py, PyAny>> {
        let attr_ref = self.cell.get_or_try_init(py, || {
            let module = py.import(self.module_name)?;
            module.getattr(self.attr_name).map(Bound::unbind)
        })?;
        Ok(attr_ref.bind(py))
    }

    #[inline]
    pub fn is_instance(&self, py: Python<'_>, value: &Bound<'_, PyAny>) -> bool {
        self.get(py)
            .and_then(|cls| value.is_instance(cls))
            .unwrap_or(false)
    }

    #[inline]
    pub fn is(&self, py: Python<'_>, value: &Bound<'_, PyAny>) -> bool {
        self.get(py).is_ok_and(|target| value.is(target))
    }
}

pub struct LazyPyNestedAttr {
    module_name: &'static str,
    parent_attr: &'static str,
    child_attr: &'static str,
    cell: PyOnceLock<Py<PyAny>>,
}

impl LazyPyNestedAttr {
    #[must_use]
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
    pub fn get<'a, 'py>(&'a self, py: Python<'py>) -> PyResult<&'a Bound<'py, PyAny>> {
        let attr_ref = self.cell.get_or_try_init(py, || {
            let module = py.import(self.module_name)?;
            let parent = module.getattr(self.parent_attr)?;
            parent.getattr(self.child_attr).map(Bound::unbind)
        })?;
        Ok(attr_ref.bind(py))
    }

    #[inline]
    pub fn is(&self, py: Python<'_>, value: &Bound<'_, PyAny>) -> bool {
        self.get(py).is_ok_and(|target| value.is(target))
    }
}

#[macro_export]
macro_rules! cached_py_import {
    // Module
    ($vis:vis $name:ident, $module:literal) => {
        $vis static $name: $crate::lazy::LazyPyModule = $crate::lazy::LazyPyModule::new($module);
    };
    // Attribute
    ($vis:vis $name:ident, $module:literal, $attr:literal) => {
        $vis static $name: $crate::lazy::LazyPyAttr = $crate::lazy::LazyPyAttr::new($module, $attr);
    };
    // Nested attribute
    ($vis:vis $name:ident, $module:literal, $parent:literal, $child:literal) => {
        $vis static $name: $crate::lazy::LazyPyNestedAttr =
            $crate::lazy::LazyPyNestedAttr::new($module, $parent, $child);
    };
}
