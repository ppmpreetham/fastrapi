use crate::runtime::py_bridge;
use bytes::{BufMut, Bytes, BytesMut};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes};

crate::cached_py_import!(TEMPFILE_MODULE, "tempfile");

const SPOOL_MAX_MEM: usize = 1024 * 1024;

#[pyclass(name = "UploadFile", module = "fastrapi.datastructures", get_all)]
pub struct PyUploadFile {
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub size: Option<u64>,
    file_content: Bytes,
    cursor: usize,
    file_obj: Option<Py<PyAny>>,
}

impl PyUploadFile {
    pub(crate) fn from_bytes(
        py: Python<'_>,
        filename: Option<String>,
        content_type: Option<String>,
        file_content: Bytes,
    ) -> Self {
        let size = file_content.len();
        let (file_content, file_obj) = if size > SPOOL_MAX_MEM {
            match spool_to_file(py, &file_content) {
                Ok(file) => (Bytes::new(), Some(file)),
                Err(err) => {
                    err.print(py);
                    (file_content, None)
                }
            }
        } else {
            (file_content, None)
        };

        Self {
            filename,
            content_type,
            size: Some(size as u64),
            file_content,
            cursor: 0,
            file_obj,
        }
    }
}

fn spool_to_file(py: Python<'_>, content: &[u8]) -> PyResult<Py<PyAny>> {
    let kwargs = pyo3::types::PyDict::new(py);
    kwargs.set_item(pyo3::intern!(py, "max_size"), SPOOL_MAX_MEM)?;
    let file = TEMPFILE_MODULE
        .get(py)?
        .getattr(pyo3::intern!(py, "SpooledTemporaryFile"))?
        .call((), Some(&kwargs))?;
    file.call_method1(pyo3::intern!(py, "write"), (content,))?;
    file.call_method1(pyo3::intern!(py, "seek"), (0,))?;
    Ok(file.unbind())
}

#[pymethods]
impl PyUploadFile {
    #[new]
    #[pyo3(signature = (file, *, size=None, filename=None, _headers=None, content_type=None))]
    fn py_new(
        py: Python<'_>,
        file: Py<PyAny>,
        size: Option<u64>,
        filename: Option<String>,
        _headers: Option<Py<PyAny>>,
        content_type: Option<String>,
    ) -> Self {
        Self {
            filename,
            content_type,
            size,
            file_content: Bytes::new(),
            cursor: 0,
            file_obj: (!file.is_none(py)).then_some(file),
        }
    }

    #[pyo3(signature = (size=-1))]
    fn read<'py>(&mut self, py: Python<'py>, size: Option<i64>) -> PyResult<Bound<'py, PyAny>> {
        let size = size.unwrap_or(-1);
        if let Some(file) = &self.file_obj {
            let file = file.clone_ref(py);
            return py_bridge::future_into_py(py, async move {
                Python::attach(|py| {
                    file.bind(py)
                        .call_method1(pyo3::intern!(py, "read"), (size,))
                        .map(|obj| obj.unbind())
                })
            });
        }

        let start = self.cursor;
        let end = if size < 0 {
            self.file_content.len()
        } else {
            std::cmp::min(self.cursor + size as usize, self.file_content.len())
        };
        let data = self.file_content.slice(start..end);
        self.cursor = end;

        py_bridge::future_into_py(py, async move {
            Python::attach(|py| Ok(PyBytes::new(py, &data).unbind()))
        })
    }

    fn write<'py>(&mut self, py: Python<'py>, data: Vec<u8>) -> PyResult<Bound<'py, PyAny>> {
        if let Some(file) = &self.file_obj {
            let file = file.clone_ref(py);
            return py_bridge::future_into_py(py, async move {
                Python::attach(|py| {
                    file.bind(py)
                        .call_method1(pyo3::intern!(py, "write"), (data,))
                        .map(|_| py.None())
                })
            });
        }

        if self.file_content.len() + data.len() > SPOOL_MAX_MEM {
            let mut buf = BytesMut::with_capacity(self.file_content.len() + data.len());
            buf.put_slice(&self.file_content);
            buf.put_slice(&data);
            let combined = buf.freeze();
            match spool_to_file(py, &combined) {
                Ok(file) => {
                    self.size = Some(combined.len() as u64);
                    self.file_content = Bytes::new();
                    self.file_obj = Some(file);
                    return py_bridge::future_into_py(py, async move { Ok(()) });
                }
                Err(err) => {
                    err.print(py);
                }
            }
        }

        let mut buf = BytesMut::with_capacity(self.file_content.len() + data.len());
        buf.put_slice(&self.file_content);
        buf.put_slice(&data);
        self.file_content = buf.freeze();
        self.size = Some(self.file_content.len() as u64);

        py_bridge::future_into_py(py, async move { Ok(()) })
    }

    fn seek<'py>(&mut self, py: Python<'py>, offset: i64) -> PyResult<Bound<'py, PyAny>> {
        if let Some(file) = &self.file_obj {
            let file = file.clone_ref(py);
            return py_bridge::future_into_py(py, async move {
                Python::attach(|py| {
                    file.bind(py)
                        .call_method1(pyo3::intern!(py, "seek"), (offset,))
                        .map(|_| py.None())
                })
            });
        }
        self.cursor = offset as usize;
        py_bridge::future_into_py(py, async move { Ok(()) })
    }

    fn close<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        if let Some(file) = &self.file_obj {
            let file = file.clone_ref(py);
            return py_bridge::future_into_py(py, async move {
                Python::attach(|py| {
                    file.bind(py)
                        .call_method0(pyo3::intern!(py, "close"))
                        .map(|_| py.None())
                })
            });
        }
        py_bridge::future_into_py(py, async move { Ok(()) })
    }
}
