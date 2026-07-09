use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes};
#[pyclass(name = "UploadFile", module = "fastrapi.datastructures")]
pub struct PyUploadFile {
    #[pyo3(get)]
    pub filename: Option<String>,
    #[pyo3(get)]
    pub content_type: Option<String>,
    #[pyo3(get)]
    pub size: Option<u64>,
    file_content: Vec<u8>,
    cursor: usize,
}

impl PyUploadFile {
    pub(crate) fn from_bytes(
        filename: Option<String>,
        content_type: Option<String>,
        file_content: Vec<u8>,
    ) -> Self {
        Self {
            filename,
            content_type,
            size: Some(file_content.len() as u64),
            file_content,
            cursor: 0,
        }
    }
}

#[pymethods]
impl PyUploadFile {
    #[new]
    #[pyo3(signature = (_file, *, size=None, filename=None, _headers=None, content_type=None))]
    fn new(
        _file: Py<PyAny>,
        size: Option<u64>,
        filename: Option<String>,
        _headers: Option<Py<PyAny>>,
        content_type: Option<String>,
    ) -> Self {
        Self {
            filename,
            content_type,
            size,
            file_content: vec![],
            cursor: 0,
        }
    }

    fn read<'py>(&mut self, py: Python<'py>, size: Option<i64>) -> PyResult<Bound<'py, PyAny>> {
        let size = size.unwrap_or(-1);
        let start = self.cursor;
        let end = if size < 0 {
            self.file_content.len()
        } else {
            std::cmp::min(self.cursor + size as usize, self.file_content.len())
        };
        let data = self.file_content[start..end].to_vec();
        self.cursor = end;

        rsloop::rust_async::future_into_py(py, async move {
            Python::attach(|py| Ok(PyBytes::new(py, &data).unbind()))
        })
    }

    fn write<'py>(&mut self, py: Python<'py>, data: Vec<u8>) -> PyResult<Bound<'py, PyAny>> {
        self.file_content.extend_from_slice(&data);
        self.size = Some(self.file_content.len() as u64);

        rsloop::rust_async::future_into_py(py, async move { Ok(()) })
    }

    fn seek<'py>(&mut self, py: Python<'py>, offset: i64) -> PyResult<Bound<'py, PyAny>> {
        self.cursor = offset as usize;
        rsloop::rust_async::future_into_py(py, async move { Ok(()) })
    }

    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        rsloop::rust_async::future_into_py(py, async move { Ok(()) })
    }
}
