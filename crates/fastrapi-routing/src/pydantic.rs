//! Integration with pydantic models.
//! This module finds pydantic models and validates Python data and JSON data with them.

use super::annotations::load_module;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyModule, PyType};
use pyo3::{Bound, Py, intern};
use simd_json::OwnedValue as Value;

fastrapi_pyutil::cached_py_import!(BASEMODEL_TYPE, "pydantic", "BaseModel");

fn get_base_model(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    BASEMODEL_TYPE.get(py)?.cast().map_err(PyErr::from)
}

pub fn is_pydantic_model(py: Python<'_>, type_hint: &Bound<'_, PyAny>) -> bool {
    let Ok(type_obj) = type_hint.cast::<PyType>() else {
        return false;
    };

    let attributes = [
        intern!(py, "model_validate"),
        intern!(py, "model_fields"),
        intern!(py, "__pydantic_validator__"),
        intern!(py, "__pydantic_core_schema__"),
    ];

    attributes
        .iter()
        .any(|&attr| type_obj.hasattr(attr).unwrap_or(false))
        || get_base_model(py).is_ok_and(|base| type_obj.is_subclass(base).unwrap_or(false))
}

#[pyfunction]
fn test_model(
    py: Python<'_>,
    module: &str,
    class_name: &str,
    data: Py<PyAny>,
) -> PyResult<Py<PyAny>> {
    let model = load_module(py, module, class_name)?;
    let bound_model = model.bind(py);
    let validated = bound_model.call1((data,))?;
    Ok(validated.into())
}

pub fn register_pydantic_integration(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(test_model, m)?)?;
    _ = get_base_model(m.py());
    Ok(())
}

const DETACHED_PARSE_MIN: usize = 64 * 1024;

pub(crate) fn parse_json(py: Python<'_>, raw: &[u8]) -> Option<Value> {
    let parse = || {
        let mut json_buf = raw.to_vec();
        simd_json::to_owned_value(&mut json_buf).ok()
    };
    if raw.len() >= DETACHED_PARSE_MIN {
        py.detach(parse)
    } else {
        parse()
    }
}
