use crate::utils::{json_to_py_object, py_to_response};
use crate::ResponseType;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use serde_json::Value;

pub fn load_pydantic_model(py: Python<'_>, module: &str, class_name: &str) -> PyResult<Py<PyAny>> {
    let module = PyModule::import(py, module)?;
    let cls = module.getattr(class_name)?;
    Ok(cls.into())
}

pub fn validate_with_pydantic<'py>(
    py: Python<'py>,
    model_class: &Bound<'py, PyAny>,
    json_payload: &Value,
) -> Result<Py<PyAny>, Response> {
    let py_data = json_to_py_object(py, json_payload);

    let validated = if let Ok(validate_method) = model_class.getattr("model_validate") {
        validate_method.call1((py_data,))
    } else {
        let data_bound = py_data.bind(py);
        if data_bound.is_instance_of::<PyDict>() {
            let dict = data_bound.cast::<PyDict>().map_err(|e| {
                let err_str = e.to_string();
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("Pydantic validation failed: {}", err_str),
                )
                    .into_response()
            })?;
            model_class.call((), Some(dict))
        } else {
            model_class.call1((py_data,))
        }
    };

    match validated {
        Ok(obj) => Ok(obj.into()),
        Err(e) => {
            e.print(py);
            let response = (StatusCode::UNPROCESSABLE_ENTITY,).into_response();
            Err(response)
        }
    }
}

pub fn call_with_pydantic_validation<'py>(
    py: Python<'py>,
    route_func: &Bound<'py, PyAny>,
    model_class: &Bound<'py, PyAny>,
    payload: &Value,
) -> Response {
    match validate_with_pydantic(py, model_class, payload) {
        Ok(validated_obj) => match route_func.call1((validated_obj,)) {
            Ok(result) => py_to_response(py, &result),
            Err(err) => {
                err.print(py);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        },
        Err(validation_error) => validation_error,
    }
}

pub fn is_pydantic_model(py: Python<'_>, type_hint: &Bound<'_, PyAny>) -> bool {
    if !type_hint.is_instance_of::<pyo3::types::PyType>() {
        return false;
    }

    // Pydantic v2: has model_validate
    if type_hint.hasattr("model_validate").unwrap_or(false) {
        return true;
    }

    // Pydantic v1: subclass of BaseModel
    if let Ok(pydantic) = PyModule::import(py, "pydantic") {
        if let Ok(base_model) = pydantic.getattr("BaseModel") {
            if let Ok(base_model_type) = base_model.cast::<pyo3::types::PyType>() {
                if let Ok(type_obj) = type_hint.cast::<pyo3::types::PyType>() {
                    return type_obj.is_subclass(&base_model_type).unwrap_or(false);
                }
            }
        }
    }

    false
}

#[pyfunction]
fn test_model(
    py: Python<'_>,
    module: String,
    class_name: String,
    data: Py<PyAny>,
) -> PyResult<Py<PyAny>> {
    let model = load_pydantic_model(py, &module, &class_name)?;
    let bound_model = model.bind(py);
    let validated = bound_model.call1((data,))?;
    Ok(validated.into())
}

pub fn register_pydantic_integration(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(test_model, m)?)?;
    Ok(())
}

pub fn parse_route_metadata(
    py: Python,
    func: &Bound<PyAny>,
) -> (Vec<(String, Py<PyAny>)>, ResponseType) {
    let mut validators = Vec::new();
    let response_type = get_response_type(py, func);

    if let Ok(annotations) = func.getattr("__annotations__") {
        if let Ok(ann_dict) = annotations.cast::<pyo3::types::PyDict>() {
            for (key, value) in ann_dict.iter() {
                if let Ok(param_name) = key.extract::<String>() {
                    if param_name != "return" && is_pydantic_model(py, &value) {
                        validators.push((param_name, value.unbind()));
                    }
                }
            }
        }
    }

    (validators, response_type)
}

fn get_response_type(_py: Python, func: &Bound<PyAny>) -> ResponseType {
    if let Ok(annotations) = func.getattr("__annotations__") {
        if let Ok(dict) = annotations.cast::<pyo3::types::PyDict>() {
            if let Ok(Some(return_type)) = dict.get_item("return") {
                let return_str = format!("{:?}", return_type);
                if return_str.contains("HTMLResponse") {
                    return ResponseType::Html;
                }
                if return_str.contains("JSONResponse") {
                    return ResponseType::Json;
                }
                if return_str.contains("PlainTextResponse") {
                    return ResponseType::PlainText;
                }
                if return_str.contains("RedirectResponse") {
                    return ResponseType::Redirect;
                }
            }
        }
    }
    ResponseType::Auto
}
