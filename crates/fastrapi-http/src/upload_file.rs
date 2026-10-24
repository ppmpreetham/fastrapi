//! This module contains the `UploadFile` class.
//! The class keeps the data of a file from a request.
//! Its `read`, `write`, `seek`, and `close` methods are asynchronous in Python.

use bytes::{BufMut, Bytes, BytesMut};
use fastrapi_runtime::py_bridge;
use pyo3::Bound;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict};
use std::cmp::min;

fastrapi_pyutil::cached_py_import!(TEMPFILE_MODULE, "tempfile");

const SPOOL_MAX_MEM: usize = 1024 * 1024;

/// A file uploaded in a request.
///
/// Define it as a *path operation function* (or dependency) parameter.
///
/// If you are using a regular `def` function, you can use the `upload_file.file`
/// attribute to access the raw standard Python file (blocking, not async), useful and
/// needed for non-async code.
///
/// Read more about it in the
/// [FastAPI docs for Request Files](https://fastapi.tiangolo.com/tutorial/request-files/).
///
/// ## Example
///
/// ```python
/// from typing import Annotated
///
/// from fastrapi import FastAPI, File, UploadFile
///
/// app = FastAPI()
///
///
/// @app.post("/files/")
/// async def create_file(file: Annotated[bytes, File()]):
///     return {"file_size": len(file)}
///
///
/// @app.post("/uploadfile/")
/// async def create_upload_file(file: UploadFile):
///     return {"filename": file.filename}
/// ```
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
    pub fn from_bytes(
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
    let kwargs = PyDict::new(py);
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

    /// Read some bytes from the file.
    ///
    /// To be awaitable, compatible with async, this is run in threadpool.
    #[pyo3(signature = (size=-1))]
    fn read<'py>(&mut self, py: Python<'py>, size: Option<i64>) -> PyResult<Bound<'py, PyAny>> {
        let size = size.unwrap_or(-1);
        if let Some(file) = &self.file_obj {
            let file = file.clone_ref(py);
            return py_bridge::future_into_py(py, async move {
                Python::attach(|py| {
                    file.bind(py)
                        .call_method1(pyo3::intern!(py, "read"), (size,))
                        .map(Bound::unbind)
                })
            });
        }

        let start = self.cursor;
        let end = if size < 0 {
            self.file_content.len()
        } else {
            min(self.cursor + size as usize, self.file_content.len())
        };
        let data = self.file_content.slice(start..end);
        self.cursor = end;

        py_bridge::future_into_py(py, async move {
            Python::attach(|py| Ok(PyBytes::new(py, &data).unbind()))
        })
    }

    /// Write some bytes to the file.
    ///
    /// You normally wouldn't use this from a file you read in a request.
    ///
    /// To be awaitable, compatible with async, this is run in threadpool.
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

    /// Move to a position in the file.
    ///
    /// Any next read or write will be done from that position.
    ///
    /// To be awaitable, compatible with async, this is run in threadpool.
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

    /// Close the file.
    ///
    /// To be awaitable, compatible with async, this is run in threadpool.
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
