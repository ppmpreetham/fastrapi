//! Integration with pydantic models.
//! This module finds pydantic models and validates Python data and JSON data with them.
//! It also converts pydantic validation errors into 422 responses.

use super::annotations::load_module;
use crate::kernel_validation::maybe_validate_with_kernel;
use crate::route::PydanticValidator;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use fastrapi_pyutil::json::{json_to_py_object, py_to_response};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyModule, PyType};
use pyo3::{Bound, Py, intern};
use simd_json::OwnedValue as Value;
use std::str::from_utf8;

pub fn pydantic_error_to_response(py: Python<'_>, err: &pyo3::PyErr) -> Response {
    use axum::response::IntoResponse;
    use fastrapi_pyutil::json::py_json_response_with_status;
    use pyo3::types::{PyDict, PyList};

    let value = err.value(py);
    if let Ok(errors) = value.call_method0(pyo3::intern!(py, "errors"))
        && let Ok(list) = errors.cast::<PyList>()
    {
        let detail = PyList::empty(py);
        for item in list.iter() {
            let Ok(entry) = item.cast::<PyDict>() else {
                continue;
            };
            let out = PyDict::new(py);
            for key in ["type", "loc", "msg", "input", "ctx"] {
                let Some(field) = entry.get_item(key).ok() else {
                    continue;
                };
                let Some(field) = field else {
                    continue;
                };
                if field.is_none() && key != "input" {
                    continue;
                }
                if key == "loc" {
                    let rooted = PyList::empty(py);
                    if rooted.append(pyo3::intern!(py, "body")).is_ok()
                        && let Ok(mut parts) = field.try_iter()
                        && parts.try_for_each(|part| rooted.append(part?)).is_ok()
                    {
                        _ = out.set_item(key, rooted);
                        continue;
                    }
                }
                _ = out.set_item(key, field);
            }
            detail.append(out).ok();
        }

        let dict = PyDict::new(py);
        if dict.set_item(pyo3::intern!(py, "detail"), detail).is_ok()
            && let Ok(resp) =
                py_json_response_with_status(py, StatusCode::UNPROCESSABLE_ENTITY, dict.as_any())
        {
            return resp;
        }
    }

    (StatusCode::UNPROCESSABLE_ENTITY, "Validation failed").into_response()
}

pub(crate) fn validate_python_with_pydantic<'py>(
    py: Python<'py>,
    validate_fn: &Bound<'py, PyAny>,
    json_payload: &Value,
) -> Result<Py<PyAny>, Response> {
    let py_data = json_to_py_object(py, json_payload);

    let validated = validate_fn.call1((py_data,));

    match validated {
        Ok(obj) => Ok(obj.into()),
        Err(e) => Err(pydantic_error_to_response(py, &e)),
    }
}

pub fn validate_json_with_pydantic(
    py: Python<'_>,
    validator: &PydanticValidator,
    raw_payload: &[u8],
) -> Result<Py<PyAny>, Response> {
    if let Some(handled) = maybe_validate_with_kernel(py, validator, raw_payload) {
        return handled;
    }

    if let Some(validate_json_method) = &validator.validate_json_method {
        let raw_str = from_utf8(raw_payload).map_err(|_| {
            (StatusCode::UNPROCESSABLE_ENTITY, "Invalid UTF-8 payload").into_response()
        })?;

        return match validate_json_method.bind(py).call1((raw_str,)) {
            Ok(obj) => Ok(obj.into()),
            Err(e) => Err(pydantic_error_to_response(py, &e)),
        };
    }

    if let Some(validate_json) = &validator.validate_json {
        let raw = PyBytes::new(py, raw_payload);
        return match validate_json.bind(py).call1((raw,)) {
            Ok(obj) => Ok(obj.into()),
            Err(e) => Err(pydantic_error_to_response(py, &e)),
        };
    }

    let mut json_buf = raw_payload.to_vec();
    let payload: Value = simd_json::to_owned_value(&mut json_buf)
        .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "Invalid JSON body").into_response())?;
    validate_python_with_pydantic(py, validator.validate_python.bind(py), &payload)
}

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

pub fn call_with_pydantic_validation<'py>(
    py: Python<'py>,
    route_func: &Bound<'py, PyAny>,
    model_class: &Bound<'py, PyAny>,
    payload: &Value,
) -> Response {
    let validate_fn = model_class
        .getattr(intern!(py, "model_validate"))
        .unwrap_or_else(|_| model_class.clone());

    match validate_python_with_pydantic(py, &validate_fn, payload) {
        Ok(validated_obj) => match route_func.call1((validated_obj,)) {
            Ok(result) => py_to_response(py, &result, StatusCode::OK),
            Err(err) => {
                err.print(py);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        },
        Err(validation_error) => validation_error,
    }
}
