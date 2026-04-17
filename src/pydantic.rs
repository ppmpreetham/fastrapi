use crate::types::response::ResponseType;
use crate::types::route::{ParameterSource, ParsedParameter, RequestInput, RouteHandler};
use crate::utils::{json_to_py_object, py_to_response};
use crate::BASEMODEL_TYPE;
use crate::{PyHTMLResponse, PyJSONResponse, PyPlainTextResponse, PyRedirectResponse};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use pyo3::types::{PyAny, PyDict, PyModule, PyString, PyTuple, PyType};
use pyo3::{intern, prelude::*};
use serde_json::{json, Value};
use std::collections::HashSet;

#[derive(Debug, Clone, Default)]
pub enum ScalarKind {
    Bool,
    Int,
    Float,
    Str,
    #[default]
    Other,
}

pub fn resolve_scalar_kind(py: Python<'_>, annotation: &Bound<'_, PyAny>) -> ScalarKind {
    let name = annotation
        .getattr(intern!(py, "__name__"))
        .ok()
        .and_then(|value| {
            value
                .cast::<PyString>()
                .ok()
                .and_then(|name| name.to_str().ok())
                .map(str::to_owned)
        })
        .or_else(|| {
            annotation
                .str()
                .ok()
                .map(|value| value.to_string_lossy().into_owned())
        })
        .unwrap_or_default()
        .to_ascii_lowercase();

    match name.as_str() {
        "bool" => ScalarKind::Bool,
        "int" => ScalarKind::Int,
        "float" => ScalarKind::Float,
        "str" => ScalarKind::Str,
        _ => ScalarKind::Other,
    }
}

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

fn initialize_basemodel(py: Python<'_>) -> Option<Py<PyType>> {
    let pydantic = py.import("pydantic").ok()?;
    let base_model = pydantic.getattr("BaseModel").ok()?;
    base_model.cast_into::<PyType>().ok().map(|ty| ty.unbind())
}

pub fn is_pydantic_model(py: Python<'_>, type_hint: &Bound<'_, PyAny>) -> bool {
    let Ok(type_obj) = type_hint.cast::<PyType>() else {
        return false;
    };

    if type_obj.hasattr("model_validate").unwrap_or(false)
        || type_obj.hasattr("model_fields").unwrap_or(false)
        || type_obj.hasattr("__pydantic_validator__").unwrap_or(false)
        || type_obj
            .hasattr("__pydantic_core_schema__")
            .unwrap_or(false)
    {
        return true;
    }

    if let Some(base_model) = BASEMODEL_TYPE.get() {
        return type_obj.is_subclass(base_model.bind(py)).unwrap_or(false);
    }

    if let Some(base_model_type) = initialize_basemodel(py) {
        let _ = BASEMODEL_TYPE.set(base_model_type);
        type_obj
            .is_subclass(BASEMODEL_TYPE.get().unwrap().bind(py))
            .unwrap_or(false)
    } else {
        false
    }
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

pub struct ParsedRouteMetadata {
    pub param_validators: Vec<(String, Py<PyAny>)>,
    pub response_type: ResponseType,
    pub path_param_names: Vec<String>,
    pub query_param_names: Vec<String>,
    pub body_param_names: Vec<String>,
    pub dependencies: Vec<crate::dependencies::DependencyNode>,
    pub dependency_needs_request: bool,
    pub parsed_params: Vec<ParsedParameter>,
    pub is_async: bool,
    pub is_fast_path: bool,
}

pub fn parse_route_metadata(py: Python, func: &Bound<PyAny>, path: &str) -> ParsedRouteMetadata {
    let response_type = get_response_type(py, func);

    let is_async = func
        .getattr("__code__")
        .and_then(|code| code.getattr("co_flags"))
        .and_then(|flags| flags.extract::<u32>())
        .map(|f| (f & 0x80) != 0)
        .unwrap_or(false);

    let path_param_names = crate::params::extract_path_param_names(path);
    let dependencies =
        crate::dependencies::parse_dependencies(py, func, &path_param_names).unwrap_or_default();

    let dependency_needs_request = dependencies.iter().any(|dep| dep.needs_request_object);
    let dep_param_names: HashSet<String> = dependencies
        .iter()
        .filter_map(|d| d.param_name.clone())
        .collect();

    let mut param_validators = Vec::new();
    let mut query_param_names = Vec::new();
    let mut body_param_names = Vec::new();
    let mut parsed_params = Vec::new();

    let _ = (|| -> PyResult<()> {
        let inspect = py.import("inspect")?;
        let signature = inspect.call_method1("signature", (func,))?;
        let parameters = signature.getattr("parameters")?;
        let items = parameters.call_method0("items")?;

        for item in items.try_iter()? {
            let item_bound = item?;
            let pair = item_bound.cast::<PyTuple>()?;

            let param_name: String = pair.get_item(0)?.cast::<PyString>()?.to_string();
            let param_obj = pair.get_item(1)?;

            if matches!(param_name.as_str(), "self" | "cls" | "return")
                || dep_param_names.contains(&param_name)
            {
                continue;
            }

            let mut parsed_param = crate::params::parse_parameter_spec(
                py,
                &param_name,
                &param_obj,
                &path_param_names,
            )?;

            if !parsed_param.is_pydantic_model {
                if let Some(ann) = &parsed_param.annotation {
                    parsed_param.scalar_kind = resolve_scalar_kind(py, &ann.bind(py));
                }
            }

            match parsed_param.source {
                ParameterSource::Query => query_param_names.push(parsed_param.name.clone()),
                ParameterSource::Body => {
                    body_param_names.push(parsed_param.name.clone());
                    if parsed_param.is_pydantic_model {
                        if let Some(ann) = &parsed_param.annotation {
                            param_validators.push((parsed_param.name.clone(), ann.clone_ref(py)));
                        }
                    }
                }
                _ => {}
            }

            parsed_params.push(parsed_param);
        }
        Ok(())
    })();

    let is_fast_path = parsed_params.is_empty() && dependencies.is_empty() && !is_async;

    ParsedRouteMetadata {
        param_validators,
        response_type,
        path_param_names,
        query_param_names,
        body_param_names,
        dependencies,
        dependency_needs_request,
        parsed_params,
        is_async,
        is_fast_path,
    }
}

fn validation_error_response(detail: impl Into<String>) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({ "detail": detail.into() })),
    )
        .into_response()
}

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" => Some(true),
        "0" | "false" | "off" | "no" => Some(false),
        _ => None,
    }
}

fn convert_scalar_value(
    py: Python<'_>,
    raw: &str,
    param: &ParsedParameter,
) -> Result<Py<PyAny>, Response> {
    match param.scalar_kind {
        ScalarKind::Bool => parse_bool(raw)
            .map(|v| {
                pyo3::types::PyBool::new(py, v)
                    .to_owned()
                    .into_any()
                    .unbind()
            })
            .ok_or_else(|| validation_error_response(format!("Invalid boolean value: {}", raw))),

        ScalarKind::Int => raw
            .parse::<i64>()
            .map(|v| v.into_pyobject(py).unwrap().into_any().unbind())
            .map_err(|_| validation_error_response(format!("Invalid integer value: {}", raw))),

        ScalarKind::Float => raw
            .parse::<f64>()
            .map(|v| v.into_pyobject(py).unwrap().into_any().unbind())
            .map_err(|_| validation_error_response(format!("Invalid number value: {}", raw))),

        ScalarKind::Str => Ok(raw.into_pyobject(py).unwrap().into_any().unbind()),

        ScalarKind::Other => {
            if let Some(ann) = param.annotation.as_ref().map(|a| a.bind(py)) {
                if let Ok(v) = ann.call1((raw,)) {
                    return Ok(v.unbind());
                }
            }
            Ok(raw.into_pyobject(py).unwrap().into_any().unbind())
        }
    }
}

fn validate_scalar_constraints(
    param: &ParsedParameter,
    value: &Bound<'_, PyAny>,
) -> Result<(), Response> {
    if let Ok(number) = value.extract::<f64>() {
        if let Some(gt) = param.constraints.gt {
            if number <= gt {
                return Err(validation_error_response(format!(
                    "{} must be greater than {}",
                    param.external_name, gt
                )));
            }
        }
        if let Some(ge) = param.constraints.ge {
            if number < ge {
                return Err(validation_error_response(format!(
                    "{} must be greater than or equal to {}",
                    param.external_name, ge
                )));
            }
        }
        if let Some(lt) = param.constraints.lt {
            if number >= lt {
                return Err(validation_error_response(format!(
                    "{} must be less than {}",
                    param.external_name, lt
                )));
            }
        }
        if let Some(le) = param.constraints.le {
            if number > le {
                return Err(validation_error_response(format!(
                    "{} must be less than or equal to {}",
                    param.external_name, le
                )));
            }
        }
    }

    if let Ok(text) = value.extract::<String>() {
        if let Some(min_length) = param.constraints.min_length {
            if text.len() < min_length {
                return Err(validation_error_response(format!(
                    "{} is shorter than {}",
                    param.external_name, min_length
                )));
            }
        }
        if let Some(max_length) = param.constraints.max_length {
            if text.len() > max_length {
                return Err(validation_error_response(format!(
                    "{} is longer than {}",
                    param.external_name, max_length
                )));
            }
        }
        if let Some(pattern) = &param.constraints.pattern {
            if !pattern.is_match(&text) {
                return Err(validation_error_response(format!(
                    "{} does not match expected pattern",
                    param.external_name
                )));
            }
        }
    }

    Ok(())
}

fn raw_value_for_parameter<'a>(
    param: &ParsedParameter,
    request_input: &'a RequestInput,
) -> Option<&'a str> {
    match param.source {
        ParameterSource::Path => request_input
            .path_params
            .get(&param.external_name)
            .or_else(|| request_input.path_params.get(&param.name))
            .map(String::as_str),
        ParameterSource::Query => request_input
            .query_params
            .get(&param.external_name)
            .or_else(|| request_input.query_params.get(&param.name))
            .map(String::as_str),
        ParameterSource::Header => request_input
            .headers
            .get(&param.external_name.to_ascii_lowercase())
            .or_else(|| request_input.headers.get(&param.external_name))
            .or_else(|| request_input.headers.get(&param.name.to_ascii_lowercase()))
            .or_else(|| request_input.headers.get(&param.name))
            .map(String::as_str),
        ParameterSource::Cookie => request_input
            .cookies
            .get(&param.external_name)
            .or_else(|| request_input.cookies.get(&param.name))
            .map(String::as_str),
        ParameterSource::Body => None,
    }
}

pub fn resolve_parameter_value(
    py: Python<'_>,
    param: &ParsedParameter,
    request_input: &RequestInput,
) -> Result<Option<Py<PyAny>>, Response> {
    let Some(raw) = raw_value_for_parameter(param, request_input) else {
        if param.has_default {
            return Ok(Some(
                param
                    .default_value
                    .as_ref()
                    .map(|v| v.clone_ref(py))
                    .unwrap_or_else(|| py.None()),
            ));
        }
        if param.required {
            return Err(validation_error_response(format!(
                "Missing required parameter: {}",
                param.external_name
            )));
        }
        return Ok(None);
    };

    let value = convert_scalar_value(py, raw, param)?;
    // validate_scalar_constraints is now pure Rust — no py needed in signature.
    validate_scalar_constraints(param, value.bind(py))?;
    Ok(Some(value))
}

fn apply_body_and_validation(
    py: Python,
    handler: &RouteHandler,
    payload: Option<&serde_json::Value>,
    kwargs: &Bound<'_, PyDict>,
) -> Result<(), Response> {
    // Build a set once — O(1) lookups below instead of O(n) linear scans.
    let known_body_params: HashSet<&str> = handler
        .body_param_names
        .iter()
        .map(String::as_str)
        .chain(handler.param_validators.iter().map(|(n, _)| n.as_str()))
        .collect();

    let body_params: Vec<&ParsedParameter> = handler
        .parsed_params
        .iter()
        .filter(|p| {
            matches!(p.source, ParameterSource::Body) || known_body_params.contains(p.name.as_str())
        })
        .collect();

    if body_params.is_empty() {
        return Ok(());
    }

    let Some(payload) = payload else {
        if body_params.iter().any(|p| p.required) {
            return Err(validation_error_response("Request body is required"));
        }
        for param in body_params {
            if param.has_default {
                let value = param
                    .default_value
                    .as_ref()
                    .map(|d| d.clone_ref(py))
                    .unwrap_or_else(|| py.None());
                kwargs.set_item(param.name.as_str(), value).ok();
            }
        }
        return Ok(());
    };

    if body_params.len() == 1 {
        let param = body_params[0];

        if param.is_pydantic_model {
            let validator = handler
                .param_validators
                .iter()
                .find(|(n, _)| n == &param.name)
                .map(|(_, v)| v.bind(py))
                .ok_or_else(|| validation_error_response("Body validator is not registered"))?;
            let validated = validate_with_pydantic(py, &validator, payload)?;
            kwargs.set_item(param.name.as_str(), validated).ok();
            return Ok(());
        }

        kwargs
            .set_item(param.name.as_str(), json_to_py_object(py, payload))
            .ok();
        return Ok(());
    }

    let obj = payload
        .as_object()
        .ok_or_else(|| validation_error_response("Body must be an object"))?;

    for param in body_params {
        let value = obj
            .get(&param.external_name)
            .or_else(|| obj.get(&param.name));

        if let Some(value) = value {
            if param.is_pydantic_model {
                let validator = handler
                    .param_validators
                    .iter()
                    .find(|(n, _)| n == &param.name)
                    .map(|(_, v)| v.bind(py))
                    .ok_or_else(|| validation_error_response("Body validator is not registered"))?;
                let validated = validate_with_pydantic(py, &validator, value)?;
                kwargs.set_item(param.name.as_str(), validated).ok();
            } else {
                kwargs
                    .set_item(param.name.as_str(), json_to_py_object(py, value))
                    .ok();
            }
            continue;
        }

        if param.has_default {
            let default_value = param
                .default_value
                .as_ref()
                .map(|d| d.clone_ref(py))
                .unwrap_or_else(|| py.None());
            kwargs.set_item(param.name.as_str(), default_value).ok();
        } else if param.required {
            return Err(validation_error_response(format!(
                "Missing field: {}",
                param.external_name
            )));
        }
    }

    Ok(())
}

pub fn apply_request_data(
    py: Python,
    handler: &RouteHandler,
    request_input: &RequestInput,
    payload: Option<&serde_json::Value>,
    kwargs: &Bound<'_, PyDict>,
) -> Result<(), Response> {
    let body_set: HashSet<&str> = handler
        .body_param_names
        .iter()
        .map(String::as_str)
        .chain(handler.param_validators.iter().map(|(n, _)| n.as_str()))
        .collect();

    for param in &handler.parsed_params {
        if matches!(param.source, ParameterSource::Body) || body_set.contains(param.name.as_str()) {
            continue;
        }
        if let Some(value) = resolve_parameter_value(py, param, request_input)? {
            kwargs.set_item(param.name.as_str(), value).ok();
        }
    }

    apply_body_and_validation(py, handler, payload, kwargs)
}

pub fn get_response_type(py: Python<'_>, func: &Bound<'_, PyAny>) -> ResponseType {
    let result: PyResult<ResponseType> = (|| {
        let annotations = func.getattr(intern!(py, "__annotations__"))?;
        let dict = annotations.cast::<PyDict>()?;

        let Some(ann) = dict.get_item(intern!(py, "return"))? else {
            return Ok(ResponseType::Json);
        };

        if ann.is(&py.get_type::<PyJSONResponse>()) {
            return Ok(ResponseType::Json);
        } else if ann.is(&py.get_type::<PyPlainTextResponse>()) {
            return Ok(ResponseType::PlainText);
        } else if ann.is(&py.get_type::<PyHTMLResponse>()) {
            return Ok(ResponseType::Html);
        } else if ann.is(&py.get_type::<PyRedirectResponse>()) {
            return Ok(ResponseType::Redirect);
        }

        let type_name_bound = if let Ok(name) = ann.getattr(intern!(py, "__name__")) {
            name
        } else {
            ann.str()?.into_any()
        };

        let name_str = type_name_bound.cast::<PyString>()?.to_str()?;

        Ok(match name_str {
            "dict" | "list" | "set" => ResponseType::Json,
            "str" => ResponseType::PlainText,
            _ if crate::pydantic::is_pydantic_model(py, &ann) => ResponseType::Json,
            _ => ResponseType::Json,
        })
    })();

    result.unwrap_or(ResponseType::Json)
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
