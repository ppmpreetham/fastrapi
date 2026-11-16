//! This module contains the `UploadFile` class.
//! The class keeps the data of a file from a request.
//! Its `read`, `write`, `seek`, and `close` methods are asynchronous in Python.

use bytes::{Bytes, BytesMut};
use fastrapi_pyutil::subclass::NativeSubclass;
use fastrapi_runtime::blocking::run_python;
use fastrapi_runtime::py_bridge;
use parking_lot::Mutex;
use pyo3::Bound;
use pyo3::exceptions::PyRuntimeError;
use pyo3::exceptions::PyValueError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict, PyType};
use std::cmp::min;
use std::mem::take;
use std::sync::Arc;

fastrapi_pyutil::cached_py_import!(TEMPFILE_MODULE, "tempfile");
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
#[pyclass(name = "UploadFile", module = "fastrapi.datastructures", subclass)]
pub struct PyUploadFile {
    #[pyo3(get, set)]
    pub filename: Option<String>,
    #[pyo3(get)]
    pub content_type: Option<String>,
    #[pyo3(get, set)]
    pub size: Option<u64>,
    file_content: Bytes,
    cursor: usize,
    file_obj: Option<Py<PyAny>>,
    headers: Option<Py<PyAny>>,
    raw_headers: Arc<[(String, String)]>,
}

static PUBLIC_TYPE: NativeSubclass = NativeSubclass::new(
    "starlette.datastructures",
    "UploadFile",
    "fastrapi.datastructures",
);

/// The `UploadFile` class users see, which is also a Starlette `UploadFile`.
pub fn upload_file_type(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    PUBLIC_TYPE.get::<PyUploadFile>(py)
}

/// Carries an upload built in Rust into the public class's constructor.
#[pyclass(frozen)]
struct ParsedUpload(Mutex<Option<PyUploadFile>>);

impl ParsedUpload {
    fn take(file: &Bound<'_, PyAny>) -> Option<PyUploadFile> {
        file.cast::<Self>().ok()?.get().0.lock().take()
    }
}

impl PyUploadFile {
    /// Creates the public `UploadFile` object for this upload.
    pub fn into_python(self, py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
        let parsed = Bound::new(py, ParsedUpload(Mutex::new(Some(self))))?;
        upload_file_type(py)?.call1((parsed,))
    }

    /// The parsed content is already in memory, so it stays there; a `SpooledTemporaryFile`
    /// is only made when Python asks for `.file` or a write grows the content past the spool limit.
    pub const fn from_bytes(
        _py: Python<'_>,
        filename: Option<String>,
        content_type: Option<String>,
        raw_headers: Arc<[(String, String)]>,
        file_content: Bytes,
    ) -> Self {
        Self {
            filename,
            content_type,
            size: Some(file_content.len() as u64),
            file_content,
            cursor: 0,
            file_obj: None,
            headers: None,
            raw_headers,
        }
    }

    /// Writes `data` at the cursor like `BytesIO.write`: it overwrites, then extends,
    /// and zero-fills a gap left by seeking past the end.
    fn write_in_memory(&mut self, data: &[u8]) {
        let mut buf = take(&mut self.file_content)
            .try_into_mut()
            .unwrap_or_else(|shared| BytesMut::from(shared.as_ref()));
        let start = self.cursor;
        if buf.len() < start {
            buf.resize(start, 0);
        }
        let overlap = min(buf.len() - start, data.len());
        buf[start..start + overlap].copy_from_slice(&data[..overlap]);
        buf.extend_from_slice(&data[overlap..]);
        self.cursor = start + data.len();
        self.file_content = buf.freeze();
    }
}

/// A `SpooledTemporaryFile` with the same memory limit Starlette uses for uploads.
fn spooled_file(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item(pyo3::intern!(py, "max_size"), SPOOL_MAX_MEM)?;
    TEMPFILE_MODULE
        .get(py)?
        .getattr(pyo3::intern!(py, "SpooledTemporaryFile"))?
        .call((), Some(&kwargs))
}

/// Runs a call on a Python file object on the blocking pool, as Starlette's `run_in_threadpool` does.
fn file_call<F>(py: Python<'_>, call: F) -> PyResult<Bound<'_, PyAny>>
where
    F: FnOnce(Python<'_>) -> PyResult<Py<PyAny>> + Send + 'static,
{
    py_bridge::future_into_py(py, async move {
        run_python(call)
            .await
            .map_err(|panicked| PyRuntimeError::new_err(panicked.to_string()))?
    })
}

#[pymethods]
impl PyUploadFile {
    #[classmethod]
    fn __get_pydantic_core_schema__<'py>(
        cls: &Bound<'py, PyType>,
        _source: &Bound<'py, PyAny>,
        _handler: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = cls.py();
        py.import(intern!(py, "pydantic_core.core_schema"))?
            .call_method1(intern!(py, "is_instance_schema"), (cls,))
    }

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
        if let Some(upload) = ParsedUpload::take(file.bind(py)) {
            return Ok(upload);
        }
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
        let file = spooled_file(py)?;
        if !self.file_content.is_empty() {
            file.call_method1(
                pyo3::intern!(py, "write"),
                (PyBytes::new(py, &self.file_content),),
            )?;
        }
        file.call_method1(pyo3::intern!(py, "seek"), (self.cursor,))?;
        let file = file.unbind();
        self.file_content = Bytes::new();
        self.file_obj = Some(file.clone_ref(py));
        Ok(file)
    }

    #[setter]
    fn set_file(&mut self, file: Py<PyAny>) {
        self.file_obj = Some(file);
    }

    #[setter]
    fn set_headers(&mut self, headers: Py<PyAny>) {
        self.headers = Some(headers);
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
            kwargs.set_item(pyo3::intern!(py, "raw"), raw)?;
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
            return file_call(py, move |py| {
                file.bind(py)
                    .call_method1(pyo3::intern!(py, "read"), (size,))
                    .map(Bound::unbind)
            });
        }

        let len = self.file_content.len();
        let start = min(self.cursor, len);
        let end = usize::try_from(size).map_or(len, |size| min(start.saturating_add(size), len));
        let data = self.file_content.slice(start..end);
        self.cursor = self.cursor.max(end);

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
        self.size = self.size.map(|size| size + data.len() as u64);
        if let Some(file) = &self.file_obj {
            let file = file.clone_ref(py);
            return file_call(py, move |py| {
                file.bind(py)
                    .call_method1(pyo3::intern!(py, "write"), (PyBytes::new(py, &data),))
                    .map(|_| py.None())
            });
        }

        if self.cursor.max(self.file_content.len()) + data.len() > SPOOL_MAX_MEM {
            let file = spooled_file(py)?.unbind();
            self.file_obj = Some(file.clone_ref(py));
            let content = take(&mut self.file_content);
            let cursor = self.cursor;
            return file_call(py, move |py| {
                let file = file.bind(py);
                file.call_method1(pyo3::intern!(py, "write"), (PyBytes::new(py, &content),))?;
                file.call_method1(pyo3::intern!(py, "seek"), (cursor,))?;
                file.call_method1(pyo3::intern!(py, "write"), (PyBytes::new(py, &data),))?;
                Ok(py.None())
            });
        }

        self.write_in_memory(&data);
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
            return file_call(py, move |py| {
                file.bind(py)
                    .call_method1(pyo3::intern!(py, "seek"), (offset,))
                    .map(|_| py.None())
            });
        }
        let Ok(cursor) = usize::try_from(offset) else {
            return py_bridge::future_into_py(py, async move {
                Err::<(), _>(PyValueError::new_err(format!(
                    "negative seek value {offset}"
                )))
            });
        };
        self.cursor = cursor;
        py_bridge::future_into_py(py, async move { Ok(()) })
    }

    /// Close the file.
    ///
    /// To be awaitable, compatible with async, this is run in threadpool.
    fn close<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        if let Some(file) = &self.file_obj {
            let file = file.clone_ref(py);
            return file_call(py, move |py| {
                file.bind(py)
                    .call_method0(pyo3::intern!(py, "close"))
                    .map(|_| py.None())
            });
        }
        py_bridge::future_into_py(py, async move { Ok(()) })
    }
}
