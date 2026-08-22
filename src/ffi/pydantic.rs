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
use crate::utils::{json_to_py_object, py_any_to_json, py_to_response};
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pyo3::types::{PyAny, PyBool, PyDict, PyFloat, PyInt, PyModule, PyString, PyTuple, PyType};
use pyo3::{intern, prelude::*};
use simd_json::OwnedValue as Value;
use simd_json::prelude::*;
use smallvec::SmallVec;
use std::borrow::Cow;
use std::collections::HashSet;

crate::cached_py_import!(INSPECT_MODULE, "inspect");

fn get_inspect(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    INSPECT_MODULE.get(py)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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

#[derive(Clone, Debug)]
pub enum ScalarCoercion {
    Single(ScalarKind),
    Union(SmallVec<[ScalarKind; 4]>),
}

pub fn resolve_scalar_coercion(py: Python<'_>, annotation: &Bound<'_, PyAny>) -> ScalarCoercion {
    let mut kinds = SmallVec::new();
    collect_scalar_kinds(py, annotation, &mut kinds);
    match kinds.as_slice() {
        [] => ScalarCoercion::Single(ScalarKind::Other),
        [only] => ScalarCoercion::Single(*only),
        _ => {
            if kinds.contains(&ScalarKind::Str) {
                ScalarCoercion::Single(ScalarKind::Str)
            } else {
                ScalarCoercion::Union(kinds)
            }
        }
    }
}

fn collect_scalar_kinds(
    py: Python<'_>,
    annotation: &Bound<'_, PyAny>,
    out: &mut SmallVec<[ScalarKind; 4]>,
) {
    if let Some(args) = union_args(py, annotation) {
        for arg in args.iter() {
            if is_none_type(&arg) {
                continue;
            }
            collect_scalar_kinds(py, &arg, out);
        }
        return;
    }
    if let Some((true, Some(element))) = Some(params::list_element_annotation(py, annotation)) {
        collect_scalar_kinds(py, &element, out);
        return;
    }
    out.push(resolve_scalar_kind(py, annotation));
}

crate::cached_py_import!(TYPING_MODULE, "typing");
crate::cached_py_import!(ENUM_MODULE, "enum");

pub fn union_args<'py>(
    py: Python<'py>,
    annotation: &Bound<'py, PyAny>,
) -> Option<Bound<'py, PyTuple>> {
    let origin = TYPING_MODULE
        .get(py)
        .ok()?
        .getattr(intern!(py, "get_origin"))
        .ok()?
        .call1((annotation,))
        .ok()?;
    if origin.is_none() {
        return None;
    }
    let is_union = TYPING_MODULE
        .get(py)
        .ok()
        .and_then(|typing| typing.getattr(intern!(py, "Union")).ok())
        .is_some_and(|marker| origin.is(&marker))
        || py
            .import(intern!(py, "types"))
            .ok()
            .and_then(|types| types.getattr(intern!(py, "UnionType")).ok())
            .is_some_and(|marker| origin.is(&marker));
    if !is_union {
        return None;
    }
    annotation
        .getattr(intern!(py, "__args__"))
        .ok()?
        .cast_into()
        .ok()
}

pub fn is_none_type(obj: &Bound<'_, PyAny>) -> bool {
    obj.getattr(intern!(obj.py(), "__name__"))
        .and_then(|name| name.extract::<String>())
        .is_ok_and(|name| name == "NoneType")
}

pub fn strip_optional<'py>(
    py: Python<'py>,
    annotation: &Bound<'py, PyAny>,
) -> Option<Bound<'py, PyAny>> {
    let args = union_args(py, annotation)?;
    let members: Vec<_> = args.iter().filter(|arg| !is_none_type(arg)).collect();
    if members.len() == 1 {
        return members.into_iter().next();
    }
    None
}

pub fn load_module(py: Python<'_>, module: &str, class_name: &str) -> PyResult<Py<PyAny>> {
    let module = PyModule::import(py, module)?;
    let cls = module.getattr(class_name)?;
    Ok(cls.into())
}

pub fn pydantic_error_to_response(py: Python<'_>, err: &pyo3::PyErr) -> axum::response::Response {
    use crate::utils::py_json_response_with_status;
    use axum::response::IntoResponse;
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
    if let Some(handled) =
        crate::ffi::kernel_validation::maybe_validate_with_kernel(py, validator, raw_payload)
    {
        return handled;
    }

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

    let mut json_buf = raw_payload.to_vec();
    let payload: Value = simd_json::to_owned_value(&mut json_buf)
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
    _ = get_base_model(m.py());
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
    let all_deps_sync = dependencies.iter().all(|dep| dep.is_sync_callable());
    let dep_param_names: HashSet<String> = dependencies
        .iter()
        .filter_map(|d| d.param_name.as_ref().map(|n| n.bind(py).to_string()))
        .collect();

    let mut param_validators = Vec::new();
    let mut body_param_names = Vec::new();
    let mut parsed_params = Vec::new();
    let mut request_param: Option<Py<PyString>> = None;

    let request_cls: Py<PyAny> = py
        .get_type::<crate::http::request::PyRequest>()
        .into_any()
        .unbind();

    _ = (|| -> PyResult<()> {
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

fn validation_error(
    loc: &[String],
    msg: impl std::fmt::Display,
    kind: &str,
    input: simd_json::OwnedValue,
) -> Response {
    let detail = simd_json::json!({
        "detail": [{
            "type": kind,
            "loc": loc.iter().map(|s| simd_json::json!(s.as_str())).collect::<Vec<_>>(),
            "msg": msg.to_string(),
            "input": input,
        }]
    });
    (StatusCode::UNPROCESSABLE_ENTITY, Json(detail)).into_response()
}

fn validation_error_response(detail: impl Into<String>) -> Response {
    validation_error(
        &["body".to_owned()],
        detail.into(),
        "value_error",
        simd_json::json!(null),
    )
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
            .map(|v| {
                pyo3::types::PyBool::new(py, v)
                    .to_owned()
                    .into_any()
                    .unbind()
            })
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
                    "loc": loc.iter().map(|s| simd_json::json!(s.as_str())).collect::<Vec<_>>(),
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
        ENUM_MODULE
            .get(py)
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
        TYPING_MODULE
            .get(py)
            .ok()
            .and_then(|typing| typing.getattr(intern!(py, "get_origin")).ok())
            .and_then(|get_origin| get_origin.call1((ann,)).ok())
            .and_then(|origin| {
                TYPING_MODULE
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
                    simd_json::json!(null),
                ));
            }
            (false, false) => None,
        });
    }

    let mut items = Vec::with_capacity(raws.len());
    for raw in &raws {
        let value = convert_scalar_value(py, raw, param)?;
        validate_scalar_constraints(py, param, value.bind(py))?;
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
                simd_json::json!(null),
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
                    &["body".to_owned()],
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
                        obj.get(&param.external_name)
                            .or_else(|| obj.get(&param.name))
                    })
                    .ok_or_else(|| {
                        validation_error(
                            &["body".to_owned(), param.external_name.clone()],
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
        BodyPayload::Json { raw, value } => match value {
            Some(payload) => payload,
            None => {
                let mut json_buf = raw.to_vec();
                parsed_storage = simd_json::to_owned_value(&mut json_buf).map_err(|_| {
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
            &["body".to_owned()],
            "Input should be a valid dictionary or object to extract fields from",
            "model_attributes_type",
            json_payload.clone(),
        )
    })?;

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
                _ = kwargs.set_item(param.name_py.bind(py), instance);
                return Ok(());
            }

            if let Some(value) = resolve_parameter_value(py, param, request_input)? {
                _ = kwargs.set_item(param.name_py.bind(py), value);
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
