//! Extracts and validates the request data for a route.
//! This module converts raw path, query, header, cookie, form, and body values into Python arguments for the endpoint.
//! When a value is not valid, it makes a 422 error response.

use super::annotations::{ScalarCoercion, ScalarKind};
use super::pydantic::{
    pydantic_error_to_response, validate_json_with_pydantic, validate_python_with_pydantic,
};
use crate::route::{ParsedParameter, RouteHandler};
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use fastrapi_core::body::{BodyField, BodyPayload};
use fastrapi_core::params::ParameterSource;
use fastrapi_http::background::PyBackgroundTasks;
use fastrapi_http::request::create_py_request;
use fastrapi_http::request_input::RequestInput;
use fastrapi_http::upload_file::PyUploadFile;
use fastrapi_pyutil::imports::{ENUM, TYPING};
use fastrapi_pyutil::json::{json_to_py_object, py_any_to_json};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBool, PyDict, PyList, PyString, PyType};
use pyo3::{Bound, Py, intern};
use simd_json::OwnedValue as Value;
use simd_json::prelude::ValueAsObject;
use smallvec::SmallVec;
use std::borrow::Cow;
use std::fmt;

#[expect(clippy::needless_pass_by_value)]
pub(crate) fn validation_error(
    loc: &[&str],
    msg: impl fmt::Display,
    kind: &str,
    input: simd_json::OwnedValue,
) -> Response {
    let detail = simd_json::json!({
        "detail": [{
            "type": kind,
            "loc": loc,
            "msg": msg.to_string(),
            "input": input
        }]
    });
    let mut response = (StatusCode::UNPROCESSABLE_ENTITY, Json(detail.clone())).into_response();
    response
        .extensions_mut()
        .insert(ValidationErrorDetails(detail));
    response
}

#[derive(Clone)]
pub struct ValidationErrorDetails(pub simd_json::OwnedValue);

fn validation_error_response(detail: impl Into<String>) -> Response {
    validation_error(
        &["body"],
        detail.into(),
        "value_error",
        simd_json::json!(null),
    )
}

impl ParsedParameter {
    pub(crate) fn error_loc(&self) -> [&str; 2] {
        let source = match self.source {
            ParameterSource::Path => "path",
            ParameterSource::Query => "query",
            ParameterSource::Header => "header",
            ParameterSource::Cookie => "cookie",
            ParameterSource::Body | ParameterSource::BackgroundTasks => "body",
        };
        [source, &self.external_name]
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
    match &param.coercion {
        ScalarCoercion::Single(kind) => convert_with_kind(py, raw, *kind, param),
        ScalarCoercion::Union(kinds) => kinds
            .iter()
            .find_map(|kind| try_convert_kind(py, raw, *kind))
            .ok_or_else(|| {
                validation_error(
                    &loc,
                    "Input should be a valid union member",
                    "union_tag_invalid",
                    simd_json::json!(raw),
                )
            }),
    }
}

fn try_convert_kind(py: Python<'_>, raw: &str, kind: ScalarKind) -> Option<Py<PyAny>> {
    match kind {
        ScalarKind::Bool => {
            parse_bool(raw).map(|parsed| PyBool::new(py, parsed).to_owned().into_any().unbind())
        }
        ScalarKind::Int => raw
            .parse::<i64>()
            .ok()
            .and_then(|parsed| parsed.into_pyobject(py).ok())
            .map(|v| v.into_any().unbind()),
        ScalarKind::Float => raw
            .parse::<f64>()
            .ok()
            .and_then(|parsed| parsed.into_pyobject(py).ok())
            .map(|v| v.into_any().unbind()),
        ScalarKind::Str => raw.into_pyobject(py).ok().map(|v| v.into_any().unbind()),
        ScalarKind::Other => None,
    }
}

fn convert_with_kind(
    py: Python<'_>,
    raw: &str,
    kind: ScalarKind,
    param: &ParsedParameter,
) -> Result<Py<PyAny>, Response> {
    let loc = param.error_loc();
    match kind {
        ScalarKind::Bool => parse_bool(raw)
            .map(|v| PyBool::new(py, v).to_owned().into_any().unbind())
            .ok_or_else(|| {
                validation_error(
                    &loc,
                    "Value could not be parsed to a boolean",
                    "bool_parsing",
                    simd_json::json!(raw),
                )
            }),

        ScalarKind::Int => {
            let parsed = raw.parse::<i64>().map_err(|_| {
                validation_error(
                    &loc,
                    "Input should be a valid integer, unable to parse string as an integer",
                    "int_parsing",
                    simd_json::json!(raw),
                )
            })?;
            parsed
                .into_pyobject(py)
                .map(|v| v.into_any().unbind())
                .map_err(|_| {
                    validation_error(
                        &loc,
                        "Input should be a valid integer",
                        "int_parsing",
                        simd_json::json!(raw),
                    )
                })
        }

        ScalarKind::Float => {
            let parsed = raw.parse::<f64>().map_err(|_| {
                validation_error(
                    &loc,
                    "Input should be a valid number, unable to parse string as a number",
                    "float_parsing",
                    simd_json::json!(raw),
                )
            })?;
            parsed
                .into_pyobject(py)
                .map(|v| v.into_any().unbind())
                .map_err(|_| {
                    validation_error(
                        &loc,
                        "Input should be a valid number",
                        "float_parsing",
                        simd_json::json!(raw),
                    )
                })
        }

        ScalarKind::Str => raw
            .into_pyobject(py)
            .map(|v| v.into_any().unbind())
            .map_err(|_| {
                validation_error(
                    &loc,
                    "Input should be a valid string",
                    "string_type",
                    simd_json::json!(raw),
                )
            }),

        ScalarKind::Other => {
            let ann = param.annotation.as_ref().map(|a| a.bind(py));
            if let Some(ann) = ann
                && let Ok(v) = ann.call1((raw,))
            {
                return Ok(v.unbind());
            }
            let (kind, msg, ctx) = annotation_rejection(py, ann);
            let detail = simd_json::json!({
                "detail": [{
                    "type": kind,
                    "loc": loc,
                    "msg": msg,
                    "input": simd_json::json!(raw),
                    "ctx": ctx,
                }]
            });
            Err((StatusCode::UNPROCESSABLE_ENTITY, Json(detail)).into_response())
        }
    }
}

fn annotation_rejection(
    py: Python<'_>,
    ann: Option<&Bound<'_, PyAny>>,
) -> (&'static str, String, simd_json::OwnedValue) {
    let Some(ann) = ann else {
        return (
            "value_error",
            "Input should be a valid value".to_string(),
            simd_json::json!({}),
        );
    };

    let is_enum = ann.cast::<PyType>().is_ok_and(|ty| {
        ENUM.get(py)
            .ok()
            .and_then(|m| m.getattr(intern!(py, "Enum")).ok())
            .and_then(|base| ty.is_subclass(base.as_any()).ok())
            .unwrap_or(false)
    });
    let members: Option<Vec<String>> = if is_enum {
        ann.try_iter().ok().map(|iter| {
            iter.filter_map(|member| {
                let member = member.ok()?;
                let value = member.getattr(intern!(py, "value")).ok()?;
                value.str().ok()?.to_str().ok().map(String::from)
            })
            .collect()
        })
    } else {
        TYPING
            .get(py)
            .ok()
            .and_then(|typing| typing.getattr(intern!(py, "get_origin")).ok())
            .and_then(|get_origin| get_origin.call1((ann,)).ok())
            .and_then(|origin| {
                TYPING
                    .get(py)
                    .ok()?
                    .getattr(intern!(py, "Literal"))
                    .ok()
                    .map(|literal| origin.is(&literal))
            })
            .filter(|is_literal| *is_literal)
            .and_then(|_| {
                ann.getattr(intern!(py, "__args__")).ok().and_then(|args| {
                    args.try_iter().ok().map(|iter| {
                        iter.filter_map(|a| a.ok()?.str().ok()?.to_str().ok().map(String::from))
                            .collect()
                    })
                })
            })
    };

    match members {
        Some(values) if !values.is_empty() => {
            let expected = values
                .iter()
                .map(|v| format!("'{v}'"))
                .collect::<Vec<_>>()
                .join(" or ");
            let kind = if is_enum { "enum" } else { "literal_error" };
            let msg = format!("Input should be {expected}");
            (kind, msg, simd_json::json!({ "expected": expected }))
        }
        _ => {
            let name = ann
                .getattr(intern!(py, "__name__"))
                .ok()
                .and_then(|name| name.extract::<String>().ok())
                .unwrap_or_else(|| "value".to_string());
            (
                "value_error",
                format!("Input should be a valid {name}"),
                simd_json::json!({}),
            )
        }
    }
}

fn validate_scalar_constraints(
    py: Python<'_>,
    param: &ParsedParameter,
    value: &Bound<'_, PyAny>,
) -> Result<(), Response> {
    let loc = param.error_loc();

    if let Ok(number) = value.extract::<f64>() {
        let numeric =
            |kind: &str, msg: String| validation_error(&loc, msg, kind, py_any_to_json(py, value));
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
                py_any_to_json(py, value),
            ));
        }
        if let Some(max_length) = param.constraints.max_length
            && text.len() > max_length
        {
            return Err(validation_error(
                &loc,
                format!("String should have at most {max_length} characters"),
                "too_long",
                py_any_to_json(py, value),
            ));
        }
        if let Some(pattern) = &param.constraints.pattern
            && !pattern.is_match(text)
        {
            return Err(validation_error(
                &loc,
                "String should match pattern",
                "string_pattern_mismatch",
                py_any_to_json(py, value),
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
) -> SmallVec<[Cow<'a, str>; 4]> {
    let matches_name = |key: &str| key == param.external_name || key == param.name;

    match param.source {
        ParameterSource::Query => request_input
            .get_all_query_params()
            .iter()
            .filter(|(k, _)| matches_name(k))
            .map(|(_, v)| v.clone())
            .collect(),
        ParameterSource::Header | ParameterSource::Cookie | ParameterSource::Path => {
            raw_value_for_parameter(param, request_input)
                .into_iter()
                .collect()
        }
        ParameterSource::Body | ParameterSource::BackgroundTasks => SmallVec::new(),
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
                    .map_or_else(|| py.None(), |v| v.clone_ref(py)),
            ),
            (false, true) => {
                return Err(validation_error(
                    &param.error_loc(),
                    "Field required",
                    "missing",
                    simd_json::json!(null),
                ));
            }
            (false, false) => None,
        });
    }

    let mut items: SmallVec<[Py<PyAny>; 4]> = SmallVec::with_capacity(raws.len());
    for raw in &raws {
        let value = convert_scalar_value(py, raw, param)?;
        validate_scalar_constraints(py, param, value.bind(py))?;
        items.push(value);
    }

    PyList::new(py, items)
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
    if param.is_pydantic_model && matches!(param.source, ParameterSource::Query) {
        let values = PyDict::new(py);
        for (name, value) in request_input.get_all_query_params() {
            let value = PyString::new(py, value);
            if let Ok(Some(existing)) = values.get_item(&**name) {
                let list = if let Ok(list) = existing.cast::<PyList>() {
                    list.clone()
                } else {
                    let list = PyList::new(py, [existing])
                        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
                    values
                        .set_item(&**name, &list)
                        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
                    list
                };
                list.append(value)
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
            } else {
                values
                    .set_item(name.as_ref(), value)
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
            }
        }
        let model = param
            .annotation
            .as_ref()
            .ok_or_else(|| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
        let result = model
            .bind(py)
            .call_method1("model_validate", (&values,))
            .or_else(|_| model.bind(py).call1((&values,)))
            .map_err(|err| pydantic_error_to_response(py, &err))?;
        return Ok(Some(result.unbind()));
    }
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
                    .map_or_else(|| py.None(), |v| v.clone_ref(py)),
            ));
        }
        if param.required {
            return Err(validation_error(
                &loc,
                "Field required",
                "missing",
                simd_json::json!(null),
            ));
        }
        return Ok(None);
    };

    let value = convert_scalar_value(py, &raw, param)?;
    validate_scalar_constraints(py, param, value.bind(py))?;
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
                py,
                file.filename.clone(),
                file.content_type.clone(),
                file.content.clone(),
            ),
        )
        .map(Py::into_any)
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
                &["body"],
                "Field required",
                "missing",
                simd_json::json!(null),
            ));
        }
        handler.payload.body_param_indices.iter().for_each(|&idx| {
            let param = &handler.payload.parsed_params[idx];
            if param.has_default {
                let value = param
                    .default_value
                    .as_ref()
                    .map_or_else(|| py.None(), |d| d.clone_ref(py));
                _ = kwargs.set_item(param.name_py.bind(py), value);
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
                    &["body"],
                    "Body validator is not registered",
                    "value_error",
                    simd_json::json!(null),
                )
            })?;

            let validator = &handler.validation.param_validators[idx];

            if param.embed {
                let mut json_buf = raw.to_vec();
                let value: Value = simd_json::to_owned_value(&mut json_buf).map_err(|_| {
                    (StatusCode::UNPROCESSABLE_ENTITY, "Invalid JSON body").into_response()
                })?;
                let inner = value
                    .as_object()
                    .and_then(|obj| {
                        obj.get(param.external_name.as_str())
                            .or_else(|| obj.get(&param.name))
                    })
                    .ok_or_else(|| {
                        validation_error(
                            &["body", &param.external_name],
                            "Field required",
                            "missing",
                            value.clone(),
                        )
                    })?;
                let validated =
                    validate_python_with_pydantic(py, validator.validate_python.bind(py), inner)?;
                kwargs.set_item(param.name_py.bind(py), validated).ok();
                return Ok(());
            }

            let validated = validate_json_with_pydantic(py, validator, raw)?;
            kwargs.set_item(param.name_py.bind(py), validated).ok();
            return Ok(());
        }
    }

    let parsed_storage;
    let json_payload = match payload {
        BodyPayload::Json { raw, value } => {
            if let Some(payload) = value {
                payload
            } else {
                let mut json_buf = raw.to_vec();
                parsed_storage = simd_json::to_owned_value(&mut json_buf).map_err(|_| {
                    (StatusCode::UNPROCESSABLE_ENTITY, "Invalid JSON body").into_response()
                })?;
                &parsed_storage
            }
        }
        BodyPayload::Form { fields: form, .. } => {
            for &idx in &handler.payload.body_param_indices {
                let param = &handler.payload.parsed_params[idx];
                let values = form
                    .get(param.external_name.as_str())
                    .or_else(|| form.get(param.name.as_str()));

                let Some(values) = values else {
                    if param.has_default {
                        let default_value = param
                            .default_value
                            .as_ref()
                            .map_or_else(|| py.None(), |d| d.clone_ref(py));
                        kwargs.set_item(param.name_py.bind(py), default_value).ok();
                    } else if param.required {
                        return Err(validation_error(
                            &["body", &param.external_name],
                            "Field required",
                            "missing",
                            simd_json::json!(null),
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
                    PyList::new(py, converted)
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
                    validate_scalar_constraints(py, param, value.bind(py))?;
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

    let obj = json_payload.as_object().ok_or_else(|| {
        validation_error(
            &["body"],
            "Input should be a valid dictionary or object to extract fields from",
            "model_attributes_type",
            json_payload.clone(),
        )
    })?;

    for &idx in &handler.payload.body_param_indices {
        let param = &handler.payload.parsed_params[idx];
        let value = obj
            .get(param.external_name.as_str())
            .or_else(|| obj.get(&param.name));

        if let Some(value) = value {
            if param.is_pydantic_model {
                let idx = param.validator_index.ok_or_else(|| {
                    validation_error(
                        &["body"],
                        "Body validator is not registered",
                        "value_error",
                        simd_json::json!(null),
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
                .map_or_else(|| py.None(), |d| d.clone_ref(py));
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
) -> Result<Option<Py<PyBackgroundTasks>>, Response> {
    if handler.payload.has_multiple_query_params {
        request_input.get_all_query_params();
    }

    if let Some(name) = &handler.payload.request_param {
        let fail = |err: PyErr| -> Response {
            err.print(py);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        };
        let req = create_py_request(
            py,
            request_input,
            payload.and_then(|p| match p {
                BodyPayload::Json { raw, .. } => Some(raw.as_ref()),
                BodyPayload::Form { .. } => None,
            }),
        )
        .map_err(fail)?;
        kwargs.set_item(name.bind(py), req).map_err(fail)?;
    }

    let mut bg_tasks_instance: Option<Py<PyBackgroundTasks>> = None;

    handler
        .payload
        .parsed_params
        .iter()
        .try_for_each(|param| -> Result<(), Response> {
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
                    let bg = Py::new(py, PyBackgroundTasks::new()).map_err(|_| {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "Failed to initialize BackgroundTasks",
                        )
                            .into_response()
                    })?;
                    bg_tasks_instance = Some(bg.clone());
                    bg
                };
                _ = kwargs.set_item(param.name_py.bind(py), instance);
                return Ok(());
            }

            if let Some(value) = resolve_parameter_value(py, param, request_input)? {
                _ = kwargs.set_item(param.name_py.bind(py), value);
            }

            Ok(())
        })?;

    apply_body_and_validation(py, handler, payload, kwargs)?;
    Ok(bg_tasks_instance)
}
