//! Port of `fastapi.security.utils`.

use pyo3::prelude::*;

/// Split an `Authorization` header into `(scheme, param)`.
#[pyfunction]
#[must_use]
pub fn get_authorization_scheme_param(
    authorization_header_value: Option<&str>,
) -> (String, String) {
    let Some(value) = authorization_header_value.filter(|v| !v.is_empty()) else {
        return (String::new(), String::new());
    };
    match value.split_once(' ') {
        Some((scheme, param)) => (scheme.to_string(), param.trim().to_string()),
        None => (value.to_string(), String::new()),
    }
}

/// `utils` submodule
pub fn register_utils_submodule(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let utils = PyModule::new(parent.py(), "utils")?;
    utils.add_function(pyo3::wrap_pyfunction!(
        get_authorization_scheme_param,
        &utils
    )?)?;
    parent.add_submodule(&utils)?;
    Ok(())
}
