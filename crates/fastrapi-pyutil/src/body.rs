use bytes::Bytes;
use pyo3::prelude::*;
use pyo3::pybacked::{PyBackedBytes, PyBackedStr};
use pyo3::types::{PyBytes, PyString};

pub const ZERO_COPY_MIN: usize = 4096;

#[must_use]
pub fn bytes_body(value: &Bound<'_, PyBytes>) -> Bytes {
    let data = value.as_bytes();
    if data.len() < ZERO_COPY_MIN {
        return Bytes::copy_from_slice(data);
    }
    Bytes::from_owner(PyBackedBytes::from(value.clone()))
}

pub fn str_body(value: &Bound<'_, PyString>) -> PyResult<Bytes> {
    let text = value.to_str()?;
    if text.len() < ZERO_COPY_MIN {
        return Ok(Bytes::copy_from_slice(text.as_bytes()));
    }
    Ok(Bytes::from_owner(PyBackedStr::try_from(value.clone())?))
}
