//! This module parses the body of a form request.
//! It reads `multipart/form-data` and `application/x-www-form-urlencoded` data and makes a `FormData` object for Python.

use crate::form_data::PyFormData;
use crate::upload_file::PyUploadFile;
use bytes::Bytes;
use fastrapi_runtime::py_bridge;
use futures_util::stream::once;
use multer::Multipart;
use pyo3::Py;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyString, PyTuple};
use std::sync::{Arc, OnceLock};

const MULTIPART: &str = "multipart/form-data";
const URLENCODED: &str = "application/x-www-form-urlencoded";

pub(crate) enum FormEntry {
    Text(String, String),
    File {
        name: String,
        filename: String,
        content_type: Option<String>,
        headers: Vec<(String, String)>,
        content: Bytes,
    },
}

#[derive(Clone, Copy)]
pub(crate) struct FormLimits {
    pub files: f64,
    pub fields: f64,
    pub part_size: usize,
}

impl Default for FormLimits {
    fn default() -> Self {
        Self {
            files: 1000.0,
            fields: 1000.0,
            part_size: 1024 * 1024,
        }
    }
}

impl FormLimits {
    fn count(limit: f64, count: &mut usize, kind: &str) -> Result<(), String> {
        *count += 1;
        if *count as f64 > limit {
            return Err(format!(
                "Too many {kind}. Maximum number of {kind} is {}.",
                Self::display(limit)
            ));
        }
        Ok(())
    }

    fn display(limit: f64) -> String {
        if limit.fract() == 0.0 && limit.is_finite() {
            format!("{limit:.0}")
        } else {
            limit.to_string()
        }
    }

    fn size(&self, size: usize, kind: &str) -> Result<(), String> {
        if size > self.part_size {
            return Err(format!(
                "{kind} exceeded maximum size of {}KB.",
                self.part_size / 1024
            ));
        }
        Ok(())
    }
}

pub(crate) async fn parse_form(
    content_type: &str,
    raw: Bytes,
    limits: FormLimits,
) -> Result<Vec<FormEntry>, String> {
    if content_type.starts_with(MULTIPART) {
        return parse_multipart(content_type, raw, limits).await;
    }
    if content_type.starts_with(URLENCODED) {
        return parse_urlencoded(&raw, limits);
    }
    Ok(Vec::new())
}

fn parse_urlencoded(raw: &[u8], limits: FormLimits) -> Result<Vec<FormEntry>, String> {
    let mut fields = 0;
    raw.split(|byte| *byte == b'&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            limits.size(pair.len() - usize::from(pair.contains(&b'=')), "Field")?;
            FormLimits::count(limits.fields, &mut fields, "fields")?;
            let (key, value) = form_urlencoded::parse(pair).next().unwrap_or_default();
            Ok(FormEntry::Text(key.into_owned(), value.into_owned()))
        })
        .collect()
}

async fn parse_multipart(
    content_type: &str,
    raw: Bytes,
    limits: FormLimits,
) -> Result<Vec<FormEntry>, String> {
    let boundary = multer::parse_boundary(content_type).map_err(|e| e.to_string())?;
    let stream = once(async move { Ok::<_, String>(raw) });
    let mut multipart = Multipart::new(stream, boundary);
    let mut entries = Vec::new();
    let (mut files, mut fields) = (0, 0);

    loop {
        let Some(field) = multipart.next_field().await.map_err(|e| e.to_string())? else {
            return Ok(entries);
        };
        let Some(name) = field.name().map(str::to_owned) else {
            return Err(
                "The Content-Disposition header field \"name\" must be provided.".to_owned(),
            );
        };
        match field.file_name() {
            Some(_) => FormLimits::count(limits.files, &mut files, "files")?,
            None => FormLimits::count(limits.fields, &mut fields, "fields")?,
        }
        let content_type = field.content_type().map(ToString::to_string);
        let headers = field
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect();
        let filename = field.file_name().map(str::to_owned);
        let content = field.bytes().await.map_err(|e| e.to_string())?;
        if filename.is_none() {
            limits.size(content.len(), "Part")?;
        }

        entries.push(match filename {
            Some(filename) => FormEntry::File {
                name,
                filename,
                content_type,
                headers,
                content,
            },
            None => FormEntry::Text(name, String::from_utf8_lossy(&content).into_owned()),
        });
    }
}

impl FormEntry {
    fn into_pair(self, py: Python<'_>) -> PyResult<(String, Py<PyAny>)> {
        Ok(match self {
            Self::Text(name, value) => (name, PyString::new(py, &value).into_any().unbind()),
            Self::File {
                name,
                filename,
                content_type,
                headers,
                content,
            } => {
                let file = PyUploadFile::from_bytes(
                    py,
                    Some(filename),
                    content_type,
                    headers.into(),
                    content,
                )
                .into_python(py)?;
                (name, file.unbind())
            }
        })
    }
}

#[pyclass(
    frozen,
    name = "AwaitableOrContextManagerWrapper",
    module = "fastrapi.request"
)]
pub struct PyFormCall {
    parsed: Py<PyAny>,
    entered: Arc<OnceLock<Py<PyAny>>>,
}

impl PyFormCall {
    pub(crate) fn new(parsed: Bound<'_, PyAny>) -> Self {
        Self {
            parsed: parsed.unbind(),
            entered: Arc::new(OnceLock::new()),
        }
    }
}

#[pymethods]
impl PyFormCall {
    fn __await__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.parsed.bind(py).call_method0(intern!(py, "__await__"))
    }

    fn __aenter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let locals = py_bridge::get_current_locals(py)?;
        let pending = py_bridge::into_future_with_locals(&locals, self.parsed.bind(py).clone())?;
        let entered = Arc::clone(&self.entered);
        py_bridge::future_into_py_with_locals(py, locals, async move {
            let form = pending.await?;
            Ok(Python::attach(|py| {
                entered.get_or_init(|| form.clone_ref(py)).clone_ref(py)
            }))
        })
    }

    #[pyo3(signature = (*_exc_info))]
    fn __aexit__<'py>(
        &self,
        py: Python<'py>,
        _exc_info: &Bound<'py, PyTuple>,
    ) -> PyResult<Bound<'py, PyAny>> {
        match self.entered.get() {
            Some(form) => form.bind(py).call_method0(intern!(py, "close")),
            None => py_bridge::future_into_py(py, async { Ok(()) }),
        }
    }
}

/// Makes a native `FormData` multidict from the form entries.
/// The multidict keeps the order of duplicate keys.
pub(crate) fn to_form_data(py: Python<'_>, entries: Vec<FormEntry>) -> PyResult<Py<PyAny>> {
    let pairs = entries
        .into_iter()
        .map(|entry| entry.into_pair(py))
        .collect::<PyResult<Vec<_>>>()?;
    Py::new(py, PyFormData::from_pairs(py, pairs)?).map(Py::into_any)
}

pub(crate) fn content_type_of(py: Python<'_>, scope: &Bound<'_, PyAny>) -> String {
    let Ok(headers) = scope.get_item(pyo3::intern!(py, "headers")) else {
        return String::new();
    };
    let value = if let Ok(headers) = headers.cast::<pyo3::types::PyDict>() {
        headers
            .get_item(PyBytes::new(py, b"content-type"))
            .ok()
            .flatten()
    } else {
        headers.try_iter().ok().and_then(|pairs| {
            pairs.flatten().find_map(|pair| {
                let name = pair.get_item(0).ok()?;
                let name = name.extract::<&[u8]>().ok()?;
                name.eq_ignore_ascii_case(b"content-type")
                    .then(|| pair.get_item(1).ok())
                    .flatten()
            })
        })
    };
    let Some(value) = value else {
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
