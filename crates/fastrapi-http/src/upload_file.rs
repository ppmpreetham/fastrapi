//! This module contains the `UploadFile` class.
//! The class keeps the data of a file from a request.
//! Its `read`, `write`, `seek`, and `close` methods are asynchronous in Python.

use bytes::{BufMut, Bytes, BytesMut};
use fastrapi_runtime::py_bridge;
use pyo3::Bound;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict};
use std::cmp::min;
use std::sync::Arc;

fastrapi_pyutil::cached_py_import!(TEMPFILE_MODULE, "tempfile");
fastrapi_pyutil::cached_py_import!(IO_MODULE, "io");
fastrapi_pyutil::cached_py_import!(STARLETTE_DATASTRUCTURES, "starlette.datastructures");

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
#[pyclass(name = "UploadFile", module = "fastrapi.datastructures")]
pub struct PyUploadFile {
    #[pyo3(get)]
    pub filename: Option<String>,
    #[pyo3(get)]
    pub content_type: Option<String>,
    #[pyo3(get)]
    pub size: Option<u64>,
    file_content: Bytes,
    cursor: usize,
    file_obj: Option<Py<PyAny>>,
    headers: Option<Py<PyAny>>,
    raw_headers: Arc<[(String, String)]>,
}

impl PyUploadFile {
    pub fn from_bytes(
        py: Python<'_>,
        filename: Option<String>,
        content_type: Option<String>,
        raw_headers: Arc<[(String, String)]>,
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
            headers: None,
            raw_headers,
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
    #[pyo3(signature = (file, *, size=None, filename=None, headers=None, content_type=None))]
    fn py_new(
        py: Python<'_>,
        file: Py<PyAny>,
        size: Option<u64>,
        filename: Option<String>,
        headers: Option<Py<PyAny>>,
        content_type: Option<String>,
    ) -> PyResult<Self> {
        let content_type = match (content_type, &headers) {
            (Some(content_type), _) => Some(content_type),
            (None, Some(headers)) => headers
                .bind(py)
                .call_method1(pyo3::intern!(py, "get"), ("content-type",))?
                .extract()?,
            (None, None) => None,
        };
        Ok(Self {
            filename,
            content_type,
            size,
            file_content: Bytes::new(),
            cursor: 0,
            file_obj: (!file.is_none(py)).then_some(file),
            headers: headers.filter(|headers| !headers.is_none(py)),
            raw_headers: Arc::from([]),
        })
    }

    #[getter]
    fn file(&mut self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        if let Some(file) = &self.file_obj {
            return Ok(file.clone_ref(py));
        }
        let file = IO_MODULE
            .get(py)?
            .getattr(pyo3::intern!(py, "BytesIO"))?
            .call1((PyBytes::new(py, &self.file_content),))?;
        file.call_method1(pyo3::intern!(py, "seek"), (self.cursor,))?;
        let file = file.unbind();
        self.file_content = Bytes::new();
        self.file_obj = Some(file.clone_ref(py));
        Ok(file)
    }

    #[getter]
    fn headers(&mut self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        if let Some(headers) = &self.headers {
            return Ok(headers.clone_ref(py));
        }
        let headers_class = STARLETTE_DATASTRUCTURES
            .get(py)?
            .getattr(pyo3::intern!(py, "Headers"))?;
        let headers = if self.raw_headers.is_empty() {
            let raw = PyDict::new(py);
            if let Some(content_type) = &self.content_type {
                raw.set_item("content-type", content_type)?;
            }
            headers_class.call1((raw,))?
        } else {
            let raw: Vec<(Bound<'_, PyBytes>, Bound<'_, PyBytes>)> = self
                .raw_headers
                .iter()
                .map(|(name, value)| {
                    (
                        PyBytes::new(py, name.as_bytes()),
                        PyBytes::new(py, value.as_bytes()),
                    )
                })
                .collect();
            let kwargs = PyDict::new(py);
            kwargs.set_item("raw", raw)?;
            headers_class.call((), Some(&kwargs))?
        }
        .unbind();
        self.headers = Some(headers.clone_ref(py));
        Ok(headers)
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
