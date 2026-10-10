//! Starlette-compatible `FormData`: an immutable multidict of text fields and
//! `UploadFile`s, preserving duplicate keys in insertion order.

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyTuple};

use crate::ffi::datastructures::PyUploadFile;
use crate::runtime::py_bridge;
use pyo3::Bound;
use pyo3::class::basic::CompareOp;
use pyo3::exceptions::PyKeyError;

#[pyclass(name = "FormData", module = "fastrapi.datastructures", get_all)]
pub struct PyFormData {
    /// Insertion-ordered pairs, duplicates included.
    items: Vec<(String, Py<PyAny>)>,
    /// Last-value-wins view, mirroring starlette's `_dict`.
    dict: Py<PyDict>,
}

impl PyFormData {
    pub(crate) fn from_pairs(py: Python<'_>, items: Vec<(String, Py<PyAny>)>) -> PyResult<Self> {
        let dict = PyDict::new(py);
        for (key, value) in &items {
            dict.set_item(key, value)?;
        }
        Ok(Self {
            items,
            dict: dict.unbind(),
        })
    }

    fn is_upload(value: &Bound<'_, PyAny>) -> bool {
        value.cast::<PyUploadFile>().is_ok()
    }
}

#[pymethods]
impl PyFormData {
    #[new]
    #[pyo3(signature = (*args, **kwargs))]
    fn py_new(
        py: Python<'_>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Self> {
        let mut items: Vec<(String, Py<PyAny>)> = Vec::new();
        for arg in args.iter() {
            append_from(&mut items, py, &arg)?;
        }
        if let Some(kwargs) = kwargs {
            for (key, value) in kwargs.iter() {
                items.push((key.extract()?, value.unbind()));
            }
        }
        Self::from_pairs(py, items)
    }

    fn getlist<'py>(&self, py: Python<'py>, key: &str) -> PyResult<Bound<'py, PyList>> {
        PyList::new(
            py,
            self.items
                .iter()
                .filter(|(item_key, _)| item_key == key)
                .map(|(_, value)| value.bind(py).clone()),
        )
    }

    fn multi_items<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let pairs: Vec<Bound<'py, PyTuple>> = self
            .items
            .iter()
            .map(|(key, value)| {
                let value = value.bind(py);
                PyTuple::new(
                    py,
                    [key.as_str().into_pyobject(py)?.as_any(), value.as_any()],
                )
            })
            .collect::<PyResult<_>>()?;
        PyList::new(py, pairs)
    }

    fn keys<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.dict.bind(py).call_method0("keys")
    }

    fn values<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.dict.bind(py).call_method0("values")
    }

    fn items<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.dict.bind(py).call_method0("items")
    }

    fn __getitem__<'py>(&self, py: Python<'py>, key: &str) -> PyResult<Bound<'py, PyAny>> {
        let value = self
            .dict
            .bind(py)
            .get_item(key)?
            .ok_or_else(|| PyKeyError::new_err(key.to_string()))?;
        Ok(value)
    }

    #[pyo3(signature = (key, default=None))]
    fn get<'py>(
        &self,
        py: Python<'py>,
        key: &str,
        default: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        Ok(self
            .dict
            .bind(py)
            .get_item(key)?
            .map(Bound::unbind)
            .or_else(|| default.map(Bound::unbind))
            .unwrap_or_else(|| py.None()))
    }

    fn __contains__(&self, key: &str) -> bool {
        self.items.iter().any(|(item_key, _)| item_key == key)
    }

    fn __iter__<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        slf.getattr("keys")?.call0()
    }

    fn __len__(&self) -> usize {
        self.items.len()
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!("FormData({})", self.multi_items(py)?.repr()?))
    }

    fn __richcmp__(
        &self,
        py: Python<'_>,
        other: &Bound<'_, PyAny>,
        op: CompareOp,
    ) -> PyResult<bool> {
        let eq = match other.cast::<Self>() {
            Ok(other) => self.sorted_eq(py, &other.borrow())?,
            Err(_) => false,
        };
        Ok(match op {
            CompareOp::Eq => eq,
            CompareOp::Ne => !eq,
            _ => false,
        })
    }

    /// Close every `UploadFile` value, mirroring starlette's async `close`.
    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let uploads: Vec<Py<PyAny>> = self
            .items
            .iter()
            .filter(|(_, value)| Self::is_upload(value.bind(py)))
            .map(|(_, value)| value.clone())
            .collect();
        // TaskLocals must be captured on the calling thread, where the loop runs.
        let locals = py_bridge::get_current_locals(py).ok();
        py_bridge::future_into_py(py, async move {
            for upload in uploads {
                let awaitable = Python::attach(|py| -> PyResult<Option<Py<PyAny>>> {
                    let value = upload.bind(py);
                    if value.cast::<PyUploadFile>().is_ok() {
                        // Our UploadFile.close() is eager: the call closes.
                        value.call_method0("close")?;
                        Ok(None)
                    } else {
                        value.call_method0("close").map(|a| Some(a.unbind()))
                    }
                })?;
                let Some(awaitable) = awaitable else {
                    continue;
                };
                let Some(locals) = &locals else {
                    continue;
                };
                let fut = Python::attach(|py| {
                    pyo3_async_runtimes::into_future_with_locals(locals, awaitable.bind(py).clone())
                })?;
                fut.await?;
            }
            Ok(())
        })
    }
}

impl PyFormData {
    /// starlette compares `sorted(self._list)`; sorting by (key, value-repr)
    /// is deterministic and never raises across mixed value types.
    fn sorted_eq(&self, py: Python<'_>, other: &PyFormData) -> PyResult<bool> {
        fn sorted_keys(form: &PyFormData, py: Python<'_>) -> PyResult<Vec<(String, String)>> {
            let mut reprs: Vec<(String, String)> = form
                .items
                .iter()
                .map(|(key, value)| Ok((key.clone(), value.bind(py).repr()?.to_string())))
                .collect::<PyResult<_>>()?;
            reprs.sort();
            Ok(reprs)
        }
        Ok(sorted_keys(self, py)? == sorted_keys(other, py)?)
    }
}

fn append_from(
    items: &mut Vec<(String, Py<PyAny>)>,
    py: Python<'_>,
    value: &Bound<'_, PyAny>,
) -> PyResult<()> {
    if let Ok(existing) = value.cast::<PyFormData>() {
        for (key, item) in &existing.borrow().items {
            items.push((key.clone(), item.bind(py).clone().unbind()));
        }
        return Ok(());
    }
    if let Ok(mapping) = value.cast::<PyDict>() {
        for (key, item) in mapping.iter() {
            items.push((key.extract()?, item.unbind()));
        }
        return Ok(());
    }
    let pairs = value
        .call_method0("multi_items")
        .or_else(|_| value.call_method0("items"))
        .unwrap_or_else(|_| value.clone());
    for pair in pairs.try_iter()? {
        let pair = pair?;
        let key: String = pair.get_item(0)?.extract()?;
        items.push((key, pair.get_item(1)?.unbind()));
    }
    Ok(())
}
