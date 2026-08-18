use crate::ffi::datastructures::PyUploadFile;
use crate::globals::BASEMODEL_TYPE;
use crate::http::responses::{
    PyFileResponse, PyHTMLResponse, PyJSONResponse, PyPlainTextResponse, PyRedirectResponse,
};
use crate::routing::dependencies::{self, DependencyNode};
use crate::routing::params;
use crate::routing::types::{
    BodyField, BodyPayload, ParameterSource, ParsedParameter, PydanticValidator, RequestInput,
    RouteHandler, SerializationHint,
};
use crate::types::response::ResponseType;
use crate::utils::{json_to_py_object, py_to_response};
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pyo3::types::{PyAny, PyBool, PyDict, PyFloat, PyInt, PyModule, PyString, PyTuple, PyType};
use pyo3::{intern, prelude::*};
use sonic_rs::{JsonContainerTrait, Value};
use std::borrow::Cow;
use std::collections::HashSet;

crate::cached_py_import!(INSPECT_MODULE, "inspect");

fn get_inspect(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    INSPECT_MODULE.get(py)
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
    if let Ok(py_type) = annotation.cast::<PyType>() {
        if py_type.is(py.get_type::<PyBool>()) {
            return ScalarKind::Bool;
        }
        if py_type.is(py.get_type::<PyInt>()) {
            return ScalarKind::Int;
        }
        if py_type.is(py.get_type::<PyFloat>()) {
            return ScalarKind::Float;
        }
        if py_type.is(py.get_type::<PyString>()) {
            return ScalarKind::Str;
        }
    }

    if annotation.is_instance_of::<PyBool>() {
        ScalarKind::Bool
    } else if annotation.is_instance_of::<PyInt>() {
        ScalarKind::Int
    } else if annotation.is_instance_of::<PyFloat>() {
        ScalarKind::Float
    } else if annotation.is_instance_of::<PyString>() {
        ScalarKind::Str
    } else {
        ScalarKind::Other
    }
}

pub fn load_module(py: Python<'_>, module: &str, class_name: &str) -> PyResult<Py<PyAny>> {
    let module = PyModule::import(py, module)?;
    let cls = module.getattr(class_name)?;
    Ok(cls.into())
}

pub fn pydantic_error_to_response(py: Python<'_>, err: &pyo3::PyErr) -> axum::response::Response {
    use crate::utils::py_json_response_with_status;
    use axum::response::IntoResponse;
    use pyo3::types::PyDict;

    let value = err.value(py);
    if let Ok(errors) = value.call_method0(pyo3::intern!(py, "errors"))
        && let dict = PyDict::new(py)
        && dict.set_item(pyo3::intern!(py, "detail"), errors).is_ok()
        && let Ok(resp) =
            py_json_response_with_status(py, StatusCode::UNPROCESSABLE_ENTITY, dict.as_any())
    {
        return resp;
    }

    (StatusCode::UNPROCESSABLE_ENTITY, "Validation failed").into_response()
}

fn validate_python_with_pydantic<'py>(
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
            Err(e) => Err(pydantic_error_to_response(py, &e)),
        };
    }

    if let Some(validate_json) = &validator.validate_json {
        let raw = pyo3::types::PyBytes::new(py, raw_payload);
        return match validate_json.bind(py).call1((raw,)) {
            Ok(obj) => Ok(obj.into()),
            Err(e) => Err(pydantic_error_to_response(py, &e)),
        };
    }

    let payload: Value = sonic_rs::from_slice(raw_payload)
        .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "Invalid JSON body").into_response())?;
    validate_python_with_pydantic(py, validator.validate_python.bind(py), &payload)
}

fn get_base_model(py: Python<'_>) -> PyResult<Bound<'_, PyType>> {
    BASEMODEL_TYPE.get(py)?.cast_into().map_err(PyErr::from)
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
        || get_base_model(py).is_ok_and(|base| type_obj.is_subclass(&base).unwrap_or(false))
}

#[pyfunction]
fn test_model(
    py: Python<'_>,
    module: String,
    class_name: String,
    data: Py<PyAny>,
) -> PyResult<Py<PyAny>> {
    let model = load_module(py, &module, &class_name)?;
    let bound_model = model.bind(py);
    let validated = bound_model.call1((data,))?;
    Ok(validated.into())
}

pub fn register_pydantic_integration(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(test_model, m)?)?;
    let _ = get_base_model(m.py());
    Ok(())
}

pub struct ParsedRouteMetadata {
    pub param_validators: Vec<PydanticValidator>,
    pub response_type: ResponseType,
    pub serialization_hint: SerializationHint,
    pub body_param_names: Vec<Py<PyString>>,
    pub request_param: Option<Py<PyString>>,
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
        .is_ok_and(|f| (f & 0x80) != 0);

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
    let mut request_param: Option<Py<PyString>> = None;

    let request_cls: Py<PyAny> = py
        .get_type::<crate::http::request::PyRequest>()
        .into_any()
        .unbind();

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

            let raw_ann = param_obj
                .getattr("annotation")
                .ok()
                .filter(|a| !params::is_inspect_empty(py, a));
            if let Some(ann) = &raw_ann {
                let ann_ref: &Bound<'_, PyAny> = ann;
                if ann_ref.is(request_cls.bind(py)) {
                    request_param = Some(pyo3::types::PyString::new(py, &param_name).unbind());
                    continue;
                }
            }

            let mut parsed_param =
                params::parse_parameter_spec(py, &param_name, &param_obj, &path_param_names)?;

            if !parsed_param.is_pydantic_model
                && let Some(ann) = &parsed_param.annotation
            {
                parsed_param.scalar_kind = if parsed_param.is_list {
                    ann.bind(py)
                        .getattr("__args__")
                        .ok()
                        .and_then(|args| args.get_item(0).ok())
                        .map(|elem| resolve_scalar_kind(py, &elem))
                        .unwrap_or(crate::ffi::pydantic::ScalarKind::Str)
                } else {
                    resolve_scalar_kind(py, ann.bind(py))
                };
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
        request_param,
        dependencies,
        dependency_needs_request,
        all_deps_sync,
        parsed_params,
        is_async,
        is_fast_path,
    }
}

fn validation_error(loc: &[String], msg: impl std::fmt::Display, kind: &str) -> Response {
    let detail = sonic_rs::json!({
        "detail": [{
            "loc": loc.iter().map(|s| sonic_rs::json!(s.as_str())).collect::<Vec<_>>(),
            "msg": msg.to_string(),
            "type": kind,
        }]
    });
    (StatusCode::UNPROCESSABLE_ENTITY, Json(detail)).into_response()
}

fn validation_error_response(detail: impl Into<String>) -> Response {
    validation_error(&["body".to_owned()], detail.into(), "value_error")
}

impl ParsedParameter {
    pub(crate) fn error_loc(&self) -> Vec<String> {
        let source = match self.source {
            ParameterSource::Path => "path",
            ParameterSource::Query => "query",
            ParameterSource::Header => "header",
            ParameterSource::Cookie => "cookie",
            ParameterSource::Body | ParameterSource::BackgroundTasks => "body",
        };
        vec![source.to_owned(), self.external_name.clone()]
    }
}

fn parse_bool(raw: &str) -> Option<bool> {
    if ["1", "true", "on", "yes"]
        .iter()
        .any(|s| raw.eq_ignore_ascii_case(s))
    {
        Some(true)
    } else {
        ["0", "false", "off", "no"]
            .iter()
            .any(|s| raw.eq_ignore_ascii_case(s))
            .then_some(false)
    }
}

fn convert_scalar_value(
    py: Python<'_>,
    raw: &str,
    param: &ParsedParameter,
) -> Result<Py<PyAny>, Response> {
    let loc = param.error_loc();
    match param.scalar_kind {
        ScalarKind::Bool => parse_bool(raw)
            .map(|v| {
                pyo3::types::PyBool::new(py, v)
                    .to_owned()
                    .into_any()
                    .unbind()
            })
            .ok_or_else(|| {
                validation_error(
                    &loc,
                    "Value could not be parsed to a boolean".to_string(),
                    "bool_parsing",
                )
            }),

        ScalarKind::Int => {
            let parsed = raw.parse::<i64>().map_err(|_| {
                validation_error(
                    &loc,
                    "Input should be a valid integer, unable to parse string as an integer",
                    "int_parsing",
                )
            })?;
            parsed
                .into_pyobject(py)
                .map(|v| v.into_any().unbind())
                .map_err(|_| {
                    validation_error(&loc, "Input should be a valid integer", "int_parsing")
                })
        }

        ScalarKind::Float => {
            let parsed = raw.parse::<f64>().map_err(|_| {
                validation_error(
                    &loc,
                    "Input should be a valid number, unable to parse string as a number",
                    "float_parsing",
                )
            })?;
            parsed
                .into_pyobject(py)
                .map(|v| v.into_any().unbind())
                .map_err(|_| {
                    validation_error(&loc, "Input should be a valid number", "float_parsing")
                })
        }

        ScalarKind::Str => raw
            .into_pyobject(py)
            .map(|v| v.into_any().unbind())
            .map_err(|_| validation_error(&loc, "Input should be a valid string", "string_type")),

        ScalarKind::Other => {
            if let Some(ann) = param.annotation.as_ref().map(|a| a.bind(py))
                && let Ok(v) = ann.call1((raw,))
            {
                return Ok(v.unbind());
            }
            raw.into_pyobject(py)
                .map(|v| v.into_any().unbind())
                .map_err(|_| validation_error(&loc, "Input should be valid", "value_error"))
        }
    }
}

fn validate_scalar_constraints(
    param: &ParsedParameter,
    value: &Bound<'_, PyAny>,
) -> Result<(), Response> {
    let loc = param.error_loc();

    if let Ok(number) = value.extract::<f64>() {
        let numeric = |kind: &str, msg: String| validation_error(&loc, msg, kind);
        if let Some(gt) = param.constraints.gt
            && number <= gt
        {
            return Err(numeric(
                "greater_than",
                format!("Input should be greater than {gt}"),
            ));
        }
        if let Some(ge) = param.constraints.ge
            && number < ge
        {
            return Err(numeric(
                "greater_than_equal",
                format!("Input should be greater than or equal to {ge}"),
            ));
        }
        if let Some(lt) = param.constraints.lt
            && number >= lt
        {
            return Err(numeric(
                "less_than",
                format!("Input should be less than {lt}"),
            ));
        }
        if let Some(le) = param.constraints.le
            && number > le
        {
            return Err(numeric(
                "less_than_equal",
                format!("Input should be less than or equal to {le}"),
            ));
        }
    }

    if let Ok(text) = value.extract::<&str>() {
        if let Some(min_length) = param.constraints.min_length
            && text.len() < min_length
        {
            return Err(validation_error(
                &loc,
                format!("String should have at least {min_length} characters"),
                "too_short",
            ));
        }
        if let Some(max_length) = param.constraints.max_length
            && text.len() > max_length
        {
            return Err(validation_error(
                &loc,
                format!("String should have at most {max_length} characters"),
                "too_long",
            ));
        }
        if let Some(pattern) = &param.constraints.pattern
            && !pattern.is_match(text)
        {
            return Err(validation_error(
                &loc,
                "String should match pattern",
                "string_pattern_mismatch",
            ));
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
        ParameterSource::Body | ParameterSource::BackgroundTasks => None,
    }
}

fn raw_values_for_list<'a>(
    param: &ParsedParameter,
    request_input: &'a RequestInput<'_>,
) -> Vec<Cow<'a, str>> {
    let matches_name = |key: &str| key == param.external_name || key == param.name;

    match param.source {
        ParameterSource::Query => request_input
            .get_all_query_params()
            .iter()
            .filter(|(k, _)| matches_name(k.as_ref()))
            .map(|(_, v)| v.clone())
            .collect(),
        ParameterSource::Header | ParameterSource::Cookie | ParameterSource::Path => {
            raw_value_for_parameter(param, request_input)
                .into_iter()
                .collect()
        }
        ParameterSource::Body | ParameterSource::BackgroundTasks => Vec::new(),
    }
}

fn resolve_list_parameter(
    py: Python<'_>,
    param: &ParsedParameter,
    request_input: &RequestInput<'_>,
) -> Result<Option<Py<PyAny>>, Response> {
    let raws = raw_values_for_list(param, request_input);

    if raws.is_empty() {
        return Ok(match (param.has_default, param.required) {
            (true, _) => Some(
                param
                    .default_value
                    .as_ref()
                    .map(|v| v.clone_ref(py))
                    .unwrap_or_else(|| py.None()),
            ),
            (false, true) => {
                return Err(validation_error(
                    &param.error_loc(),
                    "Field required",
                    "missing",
                ));
            }
            (false, false) => None,
        });
    }

    let mut items = Vec::with_capacity(raws.len());
    for raw in &raws {
        let value = convert_scalar_value(py, raw, param)?;
        validate_scalar_constraints(param, value.bind(py))?;
        items.push(value);
    }

    pyo3::types::PyList::new(py, items)
        .map_err(|err| {
            use axum::response::IntoResponse;
            err.print(py);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to build list parameter",
            )
                .into_response()
        })
        .map(|list| Some(list.into_any().unbind()))
}

pub fn resolve_parameter_value(
    py: Python<'_>,
    param: &ParsedParameter,
    request_input: &RequestInput<'_>,
) -> Result<Option<Py<PyAny>>, Response> {
    if param.is_list {
        return resolve_list_parameter(py, param, request_input);
    }

    let Some(raw) = raw_value_for_parameter(param, request_input) else {
        let loc = param.error_loc();
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
            return Err(validation_error(&loc, "Field required", "missing"));
        }
        return Ok(None);
    };

    let value = convert_scalar_value(py, &raw, param)?;
    validate_scalar_constraints(param, value.bind(py))?;
    Ok(Some(value))
}

fn form_field_to_py(
    py: Python<'_>,
    field: &BodyField,
    param: &ParsedParameter,
) -> Result<Py<PyAny>, Response> {
    match field {
        BodyField::Text(raw) => convert_scalar_value(py, raw, param),
        BodyField::File(file) => Py::new(
            py,
            PyUploadFile::from_bytes(
                file.filename.clone(),
                file.content_type.clone(),
                file.content.clone(),
            ),
        )
        .map(|upload| upload.into_any())
        .map_err(|err| {
            err.print(py);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }),
    }
}

fn apply_body_and_validation(
    py: Python,
    handler: &RouteHandler,
    payload: Option<&BodyPayload>,
    kwargs: &Bound<'_, PyDict>,
) -> Result<(), Response> {
    if handler.payload.body_param_indices.is_empty() {
        return Ok(());
    }

    let Some(payload) = payload else {
        if handler
            .payload
            .body_param_indices
            .iter()
            .any(|&idx| handler.payload.parsed_params[idx].required)
        {
            return Err(validation_error(
                &["body".to_owned()],
                "Field required",
                "missing",
            ));
        }
        handler.payload.body_param_indices.iter().for_each(|&idx| {
            let param = &handler.payload.parsed_params[idx];
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

    if handler.payload.body_param_indices.len() == 1 {
        let param = &handler.payload.parsed_params[handler.payload.body_param_indices[0]];
        if param.is_pydantic_model
            && let BodyPayload::Json { raw, .. } = payload
        {
            let idx = param.validator_index.ok_or_else(|| {
                validation_error(
                    &["body".to_owned()],
                    "Body validator is not registered",
                    "value_error",
                )
            })?;

            let validator = &handler.validation.param_validators[idx];
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
                parsed_storage = sonic_rs::from_slice(raw).map_err(|_| {
                    (StatusCode::UNPROCESSABLE_ENTITY, "Invalid JSON body").into_response()
                })?;
                &parsed_storage
            }
        },
        BodyPayload::Form(form) => {
            for &idx in &handler.payload.body_param_indices {
                let param = &handler.payload.parsed_params[idx];
                let values = form
                    .get(&param.external_name)
                    .or_else(|| form.get(&param.name));

                let Some(values) = values else {
                    if param.has_default {
                        let default_value = param
                            .default_value
                            .as_ref()
                            .map(|d| d.clone_ref(py))
                            .unwrap_or_else(|| py.None());
                        kwargs.set_item(param.name_py.bind(py), default_value).ok();
                    } else if param.required {
                        return Err(validation_error(
                            &["body".to_owned(), param.external_name.clone()],
                            "Field required",
                            "missing",
                        ));
                    }
                    continue;
                };

                let converted: Result<Vec<Py<PyAny>>, Response> = values
                    .iter()
                    .map(|v| form_field_to_py(py, v, param))
                    .collect();
                let converted = converted?;

                let value: Py<PyAny> = if param.is_list {
                    pyo3::types::PyList::new(py, converted)
                        .map_err(|err| {
                            err.print(py);
                            StatusCode::INTERNAL_SERVER_ERROR.into_response()
                        })?
                        .into_any()
                        .unbind()
                } else {
                    let value = converted
                        .into_iter()
                        .last()
                        .expect("form entry is never empty");
                    validate_scalar_constraints(param, value.bind(py))?;
                    value
                };
                kwargs.set_item(param.name_py.bind(py), value).ok();
            }

            return Ok(());
        }
    };

    if handler.payload.body_param_indices.len() == 1 {
        let param = &handler.payload.parsed_params[handler.payload.body_param_indices[0]];

        kwargs
            .set_item(param.name_py.bind(py), json_to_py_object(py, json_payload))
            .ok();
        return Ok(());
    }

    let obj = json_payload
        .as_object()
        .ok_or_else(|| validation_error_response("Body must be an object"))?;

    for &idx in &handler.payload.body_param_indices {
        let param = &handler.payload.parsed_params[idx];
        let value = obj
            .get(&param.external_name)
            .or_else(|| obj.get(&param.name));

        if let Some(value) = value {
            if param.is_pydantic_model {
                let idx = param.validator_index.ok_or_else(|| {
                    validation_error(
                        &["body".to_owned()],
                        "Body validator is not registered",
                        "value_error",
                    )
                })?;

                let validator = &handler.validation.param_validators[idx];
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
) -> Result<Option<Py<crate::engine::background::PyBackgroundTasks>>, Response> {
    if handler.payload.has_multiple_query_params {
        request_input.get_all_query_params();
    }

    if let Some(name) = &handler.payload.request_param {
        let fail = |err: PyErr| -> Response {
            err.print(py);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        };
        let req = crate::http::request::create_py_request(
            py,
            request_input,
            payload.and_then(|p| match p {
                BodyPayload::Json { raw, .. } => Some(raw.as_ref()),
                _ => None,
            }),
        )
        .map_err(fail)?;
        kwargs.set_item(name.bind(py), req).map_err(fail)?;
    }

    let mut bg_tasks_instance: Option<Py<crate::engine::background::PyBackgroundTasks>> = None;

    handler.payload.parsed_params.iter().try_for_each(
        |param| -> Result<(), axum::response::Response> {
            if matches!(param.source, ParameterSource::Body)
                || handler
                    .payload
                    .body_param_name_set
                    .contains(param.name.as_str())
            {
                return Ok(());
            }

            if matches!(param.source, ParameterSource::BackgroundTasks) {
                let instance = if let Some(bg) = &bg_tasks_instance {
                    bg.clone()
                } else {
                    let bg = Py::new(py, crate::engine::background::PyBackgroundTasks::new())
                        .map_err(|_| {
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "Failed to initialize BackgroundTasks",
                            )
                                .into_response()
                        })?;
                    bg_tasks_instance = Some(bg.clone());
                    bg
                };
                let _ = kwargs.set_item(param.name_py.bind(py), instance);
                return Ok(());
            }

            if let Some(value) = resolve_parameter_value(py, param, request_input)? {
                let _ = kwargs.set_item(param.name_py.bind(py), value);
            }

            Ok(())
        },
    )?;

    apply_body_and_validation(py, handler, payload, kwargs)?;
    Ok(bg_tasks_instance)
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
    } else if cls.is(py.get_type::<PyFileResponse>()) {
        ResponseType::File
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

        let type_name = ann
            .getattr(intern!(py, "__name__"))
            .or_else(|_| ann.str().map(|s| s.into_any()))?;
        let name_str = type_name.cast::<PyString>()?.to_str()?;

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
        } else if ann.is(py.get_type::<PyFileResponse>()) {
            return Ok(ResponseType::File);
        }

        let type_name = ann
            .getattr(intern!(py, "__name__"))
            .or_else(|_| ann.str().map(|s| s.into_any()))?;
        let name_str = type_name.cast::<PyString>()?.to_str()?;

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
            Ok(result) => py_to_response(py, &result, StatusCode::OK),
            Err(err) => {
                err.print(py);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        },
        Err(validation_error) => validation_error,
    }
}
