use crate::ffi::datastructures::PyUploadFile;
use crate::globals::BASEMODEL_TYPE;
use crate::http::responses::{
    PyHTMLResponse, PyJSONResponse, PyPlainTextResponse, PyRedirectResponse,
};
use crate::routing::dependencies::{self, DependencyNode};
use crate::routing::params;
use crate::routing::types::{
    BodyField, BodyPayload, ParameterSource, ParsedParameter, PydanticValidator, RequestInput,
    RouteHandler, SerializationHint,
};
use crate::types::response::ResponseType;
use crate::utils::utils::{json_to_py_object, py_to_response};
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use once_cell::sync::OnceCell;
use pyo3::types::{PyAny, PyBytes, PyDict, PyModule, PyString, PyTuple, PyType};
use pyo3::{intern, prelude::*};
use serde_json::{Value, json};
use std::borrow::Cow;
use std::collections::HashSet;

static INSPECT_MODULE: OnceCell<Py<PyModule>> = OnceCell::new();

fn get_inspect(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    INSPECT_MODULE
        .get_or_try_init(|| py.import(intern!(py, "inspect")).map(Bound::unbind))
        .map(|module| module.bind(py).clone())
}

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

pub fn validate_python_with_pydantic<'py>(
    py: Python<'py>,
    validate_fn: &Bound<'py, PyAny>,
    json_payload: &Value,
) -> Result<Py<PyAny>, Response> {
    let py_data = json_to_py_object(py, json_payload);

    let validated = validate_fn.call1((py_data,));

    match validated {
        Ok(obj) => Ok(obj.into()),
        Err(e) => {
            e.print(py);
            Err((StatusCode::UNPROCESSABLE_ENTITY, "Validation failed").into_response())
        }
    }
}

pub fn validate_json_with_pydantic<'py>(
    py: Python<'py>,
    validator: &PydanticValidator,
    raw_payload: &[u8],
) -> Result<Py<PyAny>, Response> {
    if let Some(validate_json_method) = &validator.validate_json_method {
        let raw_str = std::str::from_utf8(raw_payload).map_err(|_| {
            (StatusCode::UNPROCESSABLE_ENTITY, "Invalid UTF-8 payload").into_response()
        })?;

        return match validate_json_method.bind(py).call1((raw_str,)) {
            Ok(obj) => Ok(obj.into()),
            Err(e) => {
                e.print(py);
                Err((StatusCode::UNPROCESSABLE_ENTITY, "Validation failed").into_response())
            }
        };
    }

    if let Some(validate_json) = &validator.validate_json {
        let raw = pyo3::types::PyBytes::new(py, raw_payload);
        return match validate_json.bind(py).call1((raw,)) {
            Ok(obj) => Ok(obj.into()),
            Err(e) => {
                e.print(py);
                Err((StatusCode::UNPROCESSABLE_ENTITY, "Validation failed").into_response())
            }
        };
    }

    let mut buf = raw_payload.to_vec();
    let payload: Value = simd_json::serde::from_slice(&mut buf)
        .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "Invalid JSON body").into_response())?;
    validate_python_with_pydantic(py, validator.validate_python.bind(py), &payload)
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
    if BASEMODEL_TYPE.get().is_none() {
        if let Some(base_model_type) = initialize_basemodel(m.py()) {
            let _ = BASEMODEL_TYPE.set(base_model_type);
        }
    }
    Ok(())
}

pub struct ParsedRouteMetadata {
    pub param_validators: Vec<PydanticValidator>,
    pub response_type: ResponseType,
    pub serialization_hint: SerializationHint,
    pub body_param_names: Vec<Py<PyString>>,
    pub dependencies: Vec<DependencyNode>,
    pub dependency_needs_request: bool,
    pub all_deps_sync: bool,
    pub parsed_params: Vec<ParsedParameter>,
    pub is_async: bool,
    pub is_fast_path: bool,
}

pub fn parse_route_metadata(py: Python, func: &Bound<PyAny>, path: &str) -> ParsedRouteMetadata {
    let response_type = get_response_type(py, func);
    let serialization_hint = get_serialization_hint(py, func);

    let is_async = func
        .getattr("__code__")
        .and_then(|code| code.getattr("co_flags"))
        .and_then(|flags| flags.extract::<u32>())
        .map(|f| (f & 0x80) != 0)
        .unwrap_or(false);

    let path_param_names = params::extract_path_param_names(path);
    let dependencies =
        dependencies::parse_dependencies(py, func, &path_param_names).unwrap_or_default();

    let dependency_needs_request = dependencies.iter().any(|dep| dep.needs_request_object);
    let all_deps_sync = dependencies.iter().all(|dep| !dep.is_async);
    let dep_param_names: HashSet<String> = dependencies
        .iter()
        .filter_map(|d| d.param_name.clone())
        .collect();

    let mut param_validators = Vec::new();
    let mut body_param_names = Vec::new();
    let mut parsed_params = Vec::new();

    let _ = (|| -> PyResult<()> {
        let inspect = get_inspect(py)?;
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

            let mut parsed_param =
                params::parse_parameter_spec(py, &param_name, &param_obj, &path_param_names)?;

            if !parsed_param.is_pydantic_model {
                if let Some(ann) = &parsed_param.annotation {
                    parsed_param.scalar_kind = resolve_scalar_kind(py, ann.bind(py));
                }
            }

            if parsed_param.source == ParameterSource::Body {
                body_param_names.push(parsed_param.name.clone());
                if parsed_param.is_pydantic_model
                    && let Some(ann) = &parsed_param.annotation
                {
                    let validator_idx = param_validators.len();
                    parsed_param.validator_index = Some(validator_idx);

                    let ann_bound = ann.bind(py);
                    let core_validator = ann_bound
                        .getattr(intern!(py, "__pydantic_validator__"))
                        .ok()
                        .map(|v| v.unbind());
                    let validate_json = ann_bound
                        .getattr(intern!(py, "model_validate_json"))
                        .ok()
                        .map(Bound::unbind);
                    let validate_python = ann_bound
                        .getattr(intern!(py, "model_validate"))
                        .map(Bound::unbind)
                        .unwrap_or_else(|_| ann.clone_ref(py));
                    let validate_json_method = core_validator.as_ref().and_then(|core| {
                        core.bind(py)
                            .getattr(intern!(py, "validate_json"))
                            .ok()
                            .map(Bound::unbind)
                    });
                    param_validators.push(PydanticValidator {
                        name: parsed_param.name.clone(),
                        model_class: ann.clone_ref(py),
                        validate_json,
                        validate_python,
                        core_validator,
                        validate_json_method,
                    });
                }
            }

            parsed_params.push(parsed_param);
        }
        Ok(())
    })();

    let is_fast_path = parsed_params.is_empty() && dependencies.is_empty() && !is_async;

    let intern_all = |names: Vec<String>| -> Vec<Py<PyString>> {
        names
            .into_iter()
            .map(|n| PyString::new(py, &n).unbind())
            .collect()
    };
    let body_param_names = intern_all(body_param_names);

    ParsedRouteMetadata {
        param_validators,
        response_type,
        serialization_hint,
        body_param_names,
        dependencies,
        dependency_needs_request,
        all_deps_sync,
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
        if let Some(gt) = param.constraints.gt
            && number <= gt
        {
            return Err(validation_error_response(format!(
                "{} must be greater than {}",
                param.external_name, gt
            )));
        }
        if let Some(ge) = param.constraints.ge
            && number < ge
        {
            return Err(validation_error_response(format!(
                "{} must be greater than or equal to {}",
                param.external_name, ge
            )));
        }
        if let Some(lt) = param.constraints.lt
            && number >= lt
        {
            return Err(validation_error_response(format!(
                "{} must be less than {}",
                param.external_name, lt
            )));
        }
        if let Some(le) = param.constraints.le
            && number > le
        {
            return Err(validation_error_response(format!(
                "{} must be less than or equal to {}",
                param.external_name, le
            )));
        }
    }

    if let Ok(text) = value.extract::<String>() {
        if let Some(min_length) = param.constraints.min_length
            && text.len() < min_length
        {
            return Err(validation_error_response(format!(
                "{} is shorter than {}",
                param.external_name, min_length
            )));
        }
        if let Some(max_length) = param.constraints.max_length
            && text.len() > max_length
        {
            return Err(validation_error_response(format!(
                "{} is longer than {}",
                param.external_name, max_length
            )));
        }
        if let Some(pattern) = &param.constraints.pattern
            && !pattern.is_match(&text)
        {
            return Err(validation_error_response(format!(
                "{} does not match expected pattern",
                param.external_name
            )));
        }
    }

    Ok(())
}

fn raw_value_for_parameter<'a>(
    param: &ParsedParameter,
    request_input: &'a RequestInput<'_>,
) -> Option<Cow<'a, str>> {
    match param.source {
        ParameterSource::Path => request_input
            .get_path_param(&param.external_name)
            .or_else(|| request_input.get_path_param(&param.name))
            .map(Cow::Borrowed),
        ParameterSource::Query => request_input
            .get_query_param(&param.external_name)
            .or_else(|| request_input.get_query_param(&param.name)),
        ParameterSource::Header => request_input
            .get_header(&param.external_name)
            .or_else(|| request_input.get_header(&param.name))
            .map(Cow::Borrowed),
        ParameterSource::Cookie => request_input
            .get_cookie(&param.external_name)
            .or_else(|| request_input.get_cookie(&param.name))
            .map(Cow::Borrowed),
        ParameterSource::Body => None,
    }
}

pub fn resolve_parameter_value(
    py: Python<'_>,
    param: &ParsedParameter,
    request_input: &RequestInput<'_>,
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

    let value = convert_scalar_value(py, &raw, param)?;
    // validate_scalar_constraints is now pure Rust — no py needed in signature.
    validate_scalar_constraints(param, value.bind(py))?;
    Ok(Some(value))
}

fn apply_body_and_validation(
    py: Python,
    handler: &RouteHandler,
    payload: Option<&BodyPayload>,
    kwargs: &Bound<'_, PyDict>,
) -> Result<(), Response> {
    if handler.body_param_indices.is_empty() {
        return Ok(());
    }

    let Some(payload) = payload else {
        if handler
            .body_param_indices
            .iter()
            .any(|&idx| handler.parsed_params[idx].required)
        {
            return Err(validation_error_response("Request body is required"));
        }
        handler.body_param_indices.iter().for_each(|&idx| {
            let param = &handler.parsed_params[idx];
            if param.has_default {
                let value = param
                    .default_value
                    .as_ref()
                    .map(|d| d.clone_ref(py))
                    .unwrap_or_else(|| py.None());
                let _ = kwargs.set_item(param.name_py.bind(py), value);
            }
        });

        return Ok(());
    };

    if handler.body_param_indices.len() == 1 {
        let param = &handler.parsed_params[handler.body_param_indices[0]];
        if param.is_pydantic_model
            && let BodyPayload::Json { raw, .. } = payload
        {
            let idx = param
                .validator_index
                .ok_or_else(|| validation_error_response("Body validator is not registered"))?;

            let validator = &handler.param_validators[idx];
            let validated = validate_json_with_pydantic(py, validator, raw)?;
            kwargs.set_item(param.name_py.bind(py), validated).ok();
            return Ok(());
        }
    }

    let parsed_storage;
    let json_payload = match payload {
        BodyPayload::Json { raw, value } => match value {
            Some(payload) => payload,
            None => {
                let mut buf = raw.to_vec();
                parsed_storage = simd_json::serde::from_slice(&mut buf).map_err(|_| {
                    (StatusCode::UNPROCESSABLE_ENTITY, "Invalid JSON body").into_response()
                })?;
                &parsed_storage
            }
        },
        BodyPayload::Form(form) => {
            for &idx in &handler.body_param_indices {
                let param = &handler.parsed_params[idx];
                let value = form
                    .get(&param.external_name)
                    .or_else(|| form.get(&param.name));

                if let Some(value) = value {
                    match value {
                        BodyField::Text(raw) => {
                            let value = convert_scalar_value(py, &raw, param)?;
                            validate_scalar_constraints(param, value.bind(py))?;
                            kwargs.set_item(param.name_py.bind(py), value).ok();
                        }
                        BodyField::File(file) => {
                            let upload = Py::new(
                                py,
                                PyUploadFile::from_bytes(
                                    file.filename.clone(),
                                    file.content_type.clone(),
                                    file.content.clone(),
                                ),
                            )
                            .map_err(|err| {
                                err.print(py);
                                StatusCode::INTERNAL_SERVER_ERROR.into_response()
                            })?
                            .into_any();
                            kwargs.set_item(param.name_py.bind(py), upload).ok();
                        }
                    }
                    continue;
                }

                if param.has_default {
                    let default_value = param
                        .default_value
                        .as_ref()
                        .map(|d| d.clone_ref(py))
                        .unwrap_or_else(|| py.None());
                    kwargs.set_item(param.name_py.bind(py), default_value).ok();
                } else if param.required {
                    return Err(validation_error_response(format!(
                        "Missing field: {}",
                        param.external_name
                    )));
                }
            }

            return Ok(());
        }
    };

    if handler.body_param_indices.len() == 1 {
        let param = &handler.parsed_params[handler.body_param_indices[0]];

        kwargs
            .set_item(param.name_py.bind(py), json_to_py_object(py, json_payload))
            .ok();
        return Ok(());
    }

    let obj = json_payload
        .as_object()
        .ok_or_else(|| validation_error_response("Body must be an object"))?;

    for &idx in &handler.body_param_indices {
        let param = &handler.parsed_params[idx];
        let value = obj
            .get(&param.external_name)
            .or_else(|| obj.get(&param.name));

        if let Some(value) = value {
            if param.is_pydantic_model {
                let idx = param
                    .validator_index
                    .ok_or_else(|| validation_error_response("Body validator is not registered"))?;

                let validator = &handler.param_validators[idx];
                let validated =
                    validate_python_with_pydantic(py, validator.validate_python.bind(py), value)?;
                kwargs.set_item(param.name_py.bind(py), validated).ok();
            } else {
                kwargs
                    .set_item(param.name_py.bind(py), json_to_py_object(py, value))
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
            kwargs.set_item(param.name_py.bind(py), default_value).ok();
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
    request_input: &RequestInput<'_>,
    payload: Option<&BodyPayload>,
    kwargs: &Bound<'_, PyDict>,
) -> Result<(), Response> {
    let query_param_count = handler
        .parsed_params
        .iter()
        .filter(|p| matches!(p.source, ParameterSource::Query))
        .count();
    if query_param_count > 1 {
        request_input.get_all_query_params(); // populate OnceCell once
    }
    handler
        .parsed_params
        .iter()
        .try_for_each(|param| -> Result<(), axum::response::Response> {
            if matches!(param.source, ParameterSource::Body)
                || handler.body_param_name_set.contains(param.name.as_str())
            {
                return Ok(());
            }

            if let Some(value) = resolve_parameter_value(py, param, request_input)? {
                let _ = kwargs.set_item(param.name_py.bind(py), value);
            }

            Ok(())
        })?;

    apply_body_and_validation(py, handler, payload, kwargs)
}

pub fn get_response_type_from_class(py: Python<'_>, cls: &Bound<'_, PyAny>) -> ResponseType {
    if cls.is(py.get_type::<PyJSONResponse>()) {
        ResponseType::Json
    } else if cls.is(py.get_type::<PyPlainTextResponse>()) {
        ResponseType::PlainText
    } else if cls.is(py.get_type::<PyHTMLResponse>()) {
        ResponseType::Html
    } else if cls.is(py.get_type::<PyRedirectResponse>()) {
        ResponseType::Redirect
    } else {
        ResponseType::Auto
    }
}

pub fn get_serialization_hint(py: Python<'_>, func: &Bound<'_, PyAny>) -> SerializationHint {
    let result: PyResult<SerializationHint> = (|| {
        let annotations = func.getattr(intern!(py, "__annotations__"))?;
        let dict = annotations.cast::<PyDict>()?;

        let Some(ann) = dict.get_item(intern!(py, "return"))? else {
            return Ok(SerializationHint::PlainDict);
        };

        if self::is_pydantic_model(py, &ann) {
            return Ok(SerializationHint::PydanticModel);
        }

        let type_name_bound = if let Ok(name) = ann.getattr(intern!(py, "__name__")) {
            name
        } else {
            ann.str()?.into_any()
        };

        let name_str = type_name_bound.cast::<PyString>()?.to_str()?;

        Ok(match name_str {
            "dict" | "list" | "set" => SerializationHint::PlainDict,
            _ if ann
                .hasattr(intern!(py, "__dataclass_fields__"))
                .unwrap_or(false) =>
            {
                SerializationHint::Dataclass
            }
            _ => SerializationHint::Unknown,
        })
    })();

    result.unwrap_or(SerializationHint::Unknown)
}

pub fn get_response_type(py: Python<'_>, func: &Bound<'_, PyAny>) -> ResponseType {
    let result: PyResult<ResponseType> = (|| {
        let annotations = func.getattr(intern!(py, "__annotations__"))?;
        let dict = annotations.cast::<PyDict>()?;

        let Some(ann) = dict.get_item(intern!(py, "return"))? else {
            return Ok(ResponseType::Json);
        };

        if ann.is(py.get_type::<PyJSONResponse>()) {
            return Ok(ResponseType::Json);
        } else if ann.is(py.get_type::<PyPlainTextResponse>()) {
            return Ok(ResponseType::PlainText);
        } else if ann.is(py.get_type::<PyHTMLResponse>()) {
            return Ok(ResponseType::Html);
        } else if ann.is(py.get_type::<PyRedirectResponse>()) {
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
            _ if self::is_pydantic_model(py, &ann) => ResponseType::Json,
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
    let validate_fn = model_class
        .getattr(intern!(py, "model_validate"))
        .unwrap_or_else(|_| model_class.clone());

    match validate_python_with_pydantic(py, &validate_fn, payload) {
        Ok(validated_obj) => match route_func.call1((validated_obj,)) {
            Ok(result) => py_to_response(py, &result, axum::http::StatusCode::OK),
            Err(err) => {
                err.print(py);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        },
        Err(validation_error) => validation_error,
    }
}
