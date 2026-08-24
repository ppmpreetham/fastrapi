use crate::ffi::datastructures::PyUploadFile;
use bytes::Bytes;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList, PyString, PyTuple};

crate::cached_py_import!(STARLETTE_FORM_DATA, "starlette.datastructures", "FormData");

const MULTIPART: &str = "multipart/form-data";
const URLENCODED: &str = "application/x-www-form-urlencoded";

pub(crate) enum FormEntry {
    Text(String, String),
    File {
        name: String,
        filename: String,
        content_type: Option<String>,
        content: Bytes,
    },
}

pub(crate) async fn parse_form(content_type: &str, raw: &[u8]) -> Result<Vec<FormEntry>, String> {
    if content_type.starts_with(MULTIPART) {
        return parse_multipart(content_type, raw).await;
    }
    if content_type.starts_with(URLENCODED) {
        return Ok(parse_urlencoded(raw));
    }
    Ok(Vec::new())
}

fn parse_urlencoded(raw: &[u8]) -> Vec<FormEntry> {
    form_urlencoded::parse(raw)
        .map(|(key, value)| FormEntry::Text(key.into_owned(), value.into_owned()))
        .collect()
}

async fn parse_multipart(content_type: &str, raw: &[u8]) -> Result<Vec<FormEntry>, String> {
    let boundary = multer::parse_boundary(content_type).map_err(|e| e.to_string())?;
    let chunk = bytes::Bytes::copy_from_slice(raw);
    let stream = futures_util::stream::once(async move { Ok::<_, String>(chunk) });
    let mut multipart = multer::Multipart::new(stream, boundary);
    let mut entries = Vec::new();

    loop {
        let Some(field) = multipart.next_field().await.map_err(|e| e.to_string())? else {
            return Ok(entries);
        };
        let Some(name) = field.name().map(str::to_owned) else {
            continue;
        };
        let content_type = field.content_type().map(ToString::to_string);
        let filename = field.file_name().map(str::to_owned);
        let content = field.bytes().await.map_err(|e| e.to_string())?;

        entries.push(match filename {
            Some(filename) => FormEntry::File {
                name,
                filename,
                content_type,
                content,
            },
            None => FormEntry::Text(name, String::from_utf8_lossy(&content).into_owned()),
        });
    }
}

impl FormEntry {
    fn into_pair(self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let (name, value) = match self {
            Self::Text(name, value) => (name, PyString::new(py, &value).into_any().unbind()),
            Self::File {
                name,
                filename,
                content_type,
                content,
            } => {
                let file = Py::new(
                    py,
                    PyUploadFile::from_bytes(py, Some(filename), content_type, content),
                )?;
                (name, file.into_any())
            }
        };
        let pair = [name.into_pyobject(py)?.unbind().into_any(), value];
        Ok(PyTuple::new(py, pair)?.unbind().into_any())
    }
}

/// starlette hands the pairs to `FormData`, an immutable multidict that keeps duplicates.
pub(crate) fn to_form_data(py: Python<'_>, entries: Vec<FormEntry>) -> PyResult<Py<PyAny>> {
    let pairs = entries
        .into_iter()
        .map(|entry| entry.into_pair(py))
        .collect::<PyResult<Vec<_>>>()?;
    let list = PyList::new(py, pairs)?;
    Ok(STARLETTE_FORM_DATA.get(py)?.call1((list,))?.unbind())
}

pub(crate) fn content_type_of(py: Python<'_>, scope: &Bound<'_, PyAny>) -> String {
    let Ok(headers) = scope.get_item("headers") else {
        return String::new();
    };
    let Ok(value) = headers.get_item(PyBytes::new(py, b"content-type")) else {
        return String::new();
    };
    let text = value.extract::<String>().or_else(|_| {
        value
            .extract::<Vec<u8>>()
            .map(|raw| String::from_utf8_lossy(&raw).into_owned())
    });
    text.map(|text| text.trim().to_ascii_lowercase())
        .unwrap_or_default()
}
