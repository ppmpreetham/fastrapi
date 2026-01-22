use crate::utils::{json_to_py_object, py_to_response};
use crate::ResponseType;
use crate::RouteHandler;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule, PyType};
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
        if let Ok(dict) = data_bound.cast::<PyDict>() {
            model_class.call((), Some(dict))
        } else {
            model_class.call1((py_data,))
        }
    };

    match validated {
        Ok(obj) => Ok(obj.into()),
        Err(e) => {
            e.print(py);
            Err((StatusCode::UNPROCESSABLE_ENTITY, "Validation failed").into_response())
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
    if !type_hint.is_instance_of::<PyType>() {
        return false;
    }

    if type_hint.hasattr("model_validate").unwrap_or(false) {
        return true;
    }

    if type_hint.hasattr("model_fields").unwrap_or(false) {
        return true;
    }

    if let Ok(pydantic) = py.import("pydantic") {
        if let Ok(base_model) = pydantic.getattr("BaseModel") {
            if let Ok(base_model_type) = base_model.cast::<PyType>() {
                if let Ok(type_obj) = type_hint.cast::<PyType>() {
                    return type_obj.is_subclass(base_model_type).unwrap_or(false);
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
    path: &str,
) -> (
    Vec<(String, Py<PyAny>)>,                 // validators
    ResponseType,                             // response_type
    Vec<String>,                              // path_params
    Vec<String>,                              // query_params
    Vec<String>,                              // body_params
    Vec<crate::dependencies::DependencyInfo>, // dependencies
    bool,                                     // is_async
    bool,                                     // is_fast_path
) {
    let response_type = get_response_type(py, func);
    let is_async = func
        .getattr("__code__")
        .ok()
        .and_then(|code| code.getattr("co_flags").ok())
        .and_then(|flags| flags.extract::<u32>().ok())
        .map(|flags| flags & 0x80 != 0)
        .unwrap_or(false);
    let path_params = crate::params::extract_path_param_names(path);
    let dependencies = crate::dependencies::parse_dependencies(py, func).unwrap_or_default();

    let dep_param_names: std::collections::HashSet<String> = dependencies
        .iter()
        .filter_map(|d| d.param_name.clone())
        .collect();

    let mut validators = Vec::new();
    let mut query_params = Vec::new();
    let mut body_params = Vec::new();

    if let Ok(annotations) = func.getattr("__annotations__") {
        if let Ok(ann_dict) = annotations.cast::<pyo3::types::PyDict>() {
            for (key, value) in ann_dict.iter() {
                let param_name: String = match key.extract() {
                    Ok(name) => name,
                    Err(_) => continue,
                };

                if param_name == "return" {
                    continue;
                }

                // dependency params aren't handled here
                if dep_param_names.contains(&param_name) {
                    continue;
                }

                if crate::pydantic::is_pydantic_model(py, &value) {
                    validators.push((param_name.clone(), value.into()));
                    body_params.push(param_name);
                    continue;
                }

                if path_params.contains(&param_name) {
                    continue;
                }
                query_params.push(param_name);
            }
        }
    }

    let is_fast_path = validators.is_empty() && dependencies.is_empty() && !is_async;

    (
        validators,
        response_type,
        path_params,
        query_params,
        body_params,
        dependencies,
        is_async,
        is_fast_path,
    )
}

pub fn apply_body_and_validation(
    py: Python,
    handler: &RouteHandler,
    payload: &serde_json::Value,
    kwargs: &Bound<'_, PyDict>,
) -> Result<(), Response> {
    use serde_json::json;

    let obj = match payload.as_object() {
        Some(o) => o,
        None => {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "detail": "Body must be an object" })),
            )
                .into_response());
        }
    };

    if !handler.param_validators.is_empty() {
        for (name, validator) in &handler.param_validators {
            let value = obj.get(name).ok_or_else(|| {
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({ "detail": format!("Missing field: {}", name) })),
                )
                    .into_response()
            })?;

            let validator = validator.bind(py);
            let validated = crate::pydantic::validate_with_pydantic(py, validator, value)?;
            kwargs.set_item(name, validated).ok();
        }
    } else {
        for (k, v) in obj {
            let py_val = crate::utils::json_to_py_object(py, v);
            kwargs.set_item(k, py_val).ok();
        }
    }

    Ok(())
}

fn get_response_type(_py: Python, func: &Bound<PyAny>) -> ResponseType {
    if let Ok(annotations) = func.getattr("__annotations__") {
        if let Ok(dict) = annotations.cast::<pyo3::types::PyDict>() {
            if let Ok(Some(return_annotation)) = dict.get_item("return") {
                if let Ok(type_str) = return_annotation.extract::<String>() {
                    return match type_str.as_str() {
                        "HTMLResponse" => ResponseType::Html,
                        "JSONResponse" => ResponseType::Json,
                        "PlainTextResponse" => ResponseType::PlainText,
                        "RedirectResponse" => ResponseType::Redirect,
                        _ => ResponseType::Auto,
                    };
                }
            }
        }
    }
    ResponseType::Auto
}
