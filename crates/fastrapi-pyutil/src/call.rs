use pyo3::IntoPyObjectExt;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyFunction, PyString, PyTuple};
use smallvec::SmallVec;

#[derive(Debug)]
pub struct CallLayout {
    names: Box<[Py<PyString>]>,
}

impl CallLayout {
    #[must_use]
    pub fn of(func: &Bound<'_, PyAny>) -> Option<Self> {
        let py = func.py();
        if !func.is_exact_instance_of::<PyFunction>() {
            return None;
        }
        let code = func.getattr(intern!(py, "__code__")).ok()?;
        let posonly: usize = code
            .getattr(intern!(py, "co_posonlyargcount"))
            .ok()?
            .extract()
            .ok()?;
        let argcount: usize = code
            .getattr(intern!(py, "co_argcount"))
            .ok()?
            .extract()
            .ok()?;
        if posonly != 0 || argcount == 0 {
            return None;
        }
        let varnames = code
            .getattr(intern!(py, "co_varnames"))
            .ok()?
            .cast_into::<PyTuple>()
            .ok()?;
        let names = varnames
            .iter()
            .take(argcount)
            .map(|name| {
                name.cast::<PyString>()
                    .ok()
                    .and_then(|name| name.to_str().ok().map(|name| PyString::intern(py, name)))
                    .map(Bound::unbind)
            })
            .collect::<Option<Box<[_]>>>()?;
        (names.len() == argcount).then_some(Self { names })
    }
}

/// The keyword arguments of one call, in the order they were set.
///
/// Every name must be an interned string (`PyString::intern` or `intern!`), as `CallLayout`
/// names are: equal names are then the same object, so a name is found by pointer alone.
#[derive(Default)]
pub struct CallArgs {
    entries: SmallVec<[(Py<PyString>, Py<PyAny>); 8]>,
}

impl CallArgs {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn position(&self, name: &Bound<'_, PyString>) -> Option<usize> {
        let found = self
            .entries
            .iter()
            .position(|(key, _)| key.as_ptr() == name.as_ptr());
        debug_assert!(
            found.is_some()
                || !self.entries.iter().any(|(key, _)| {
                    matches!(
                        (key.bind(name.py()).to_str(), name.to_str()),
                        (Ok(key), Ok(name)) if key == name
                    )
                }),
            "CallArgs name is not interned"
        );
        found
    }

    pub fn set<'py, V>(&mut self, name: &Bound<'py, PyString>, value: V) -> PyResult<()>
    where
        V: IntoPyObject<'py>,
    {
        let value = value.into_bound_py_any(name.py())?.unbind();
        match self.position(name) {
            Some(index) => self.entries[index].1 = value,
            None => self.entries.push((name.clone().unbind(), value)),
        }
        Ok(())
    }

    #[must_use]
    pub fn get<'py>(&self, name: &Bound<'py, PyString>) -> Option<Bound<'py, PyAny>> {
        self.position(name)
            .map(|index| self.entries[index].1.bind(name.py()).clone())
    }

    pub fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        for (name, value) in &self.entries {
            dict.set_item(name.bind(py), value.bind(py))?;
        }
        Ok(dict)
    }

    pub fn call<'py>(
        &self,
        func: &Bound<'py, PyAny>,
        layout: Option<&CallLayout>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = func.py();
        if self.entries.is_empty() {
            return func.call0();
        }
        if let Some(layout) = layout
            && let Some(names) = layout.names.get(..self.entries.len())
        {
            let ordered = names
                .iter()
                .map(|name| {
                    self.position(name.bind(py))
                        .map(|index| &self.entries[index].1)
                })
                .collect::<Option<SmallVec<[&Py<PyAny>; 8]>>>();
            if let Some(ordered) = ordered {
                return match ordered.as_slice() {
                    [a] => func.call1((*a,)),
                    [a, b] => func.call1((*a, *b)),
                    [a, b, c] => func.call1((*a, *b, *c)),
                    [a, b, c, d] => func.call1((*a, *b, *c, *d)),
                    _ => func.call1(PyTuple::new(py, ordered)?),
                };
            }
        }
        func.call((), Some(&self.to_dict(py)?))
    }
}
