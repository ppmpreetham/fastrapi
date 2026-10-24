//! Analysis of a Python endpoint function when FastrAPI registers a route.
//! This module reads the function signature and makes the route metadata: the parameters, the dependencies, and the response type.
//! It also selects the serialization hint for the return value.

use super::pydantic::is_pydantic_model;
use crate::dependencies::{self, DependencyNode};
use crate::params;
use crate::route::{ParsedParameter, PydanticValidator};
use fastrapi_core::params::ParameterSource;
use fastrapi_http::request::PyRequest;
use fastrapi_http::response_type::ResponseType;
use fastrapi_http::responses::{
    PyFileResponse, PyHTMLResponse, PyJSONResponse, PyPlainTextResponse, PyRedirectResponse,
};
use fastrapi_pyutil::imports::INSPECT;
use fastrapi_pyutil::json::SerializationHint;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyString, PyTuple};
use pyo3::{Bound, Py, intern};
use std::collections::HashSet;
use std::sync::OnceLock;

pub struct ParsedRouteMetadata {
    pub param_validators: Vec<PydanticValidator>,
    pub response_type: ResponseType,
    pub serialization_hint: SerializationHint,
    pub body_param_names: Vec<Py<PyString>>,
    pub request_param: Option<Py<PyString>>,
    pub response_param: Option<Py<PyString>>,
    pub dependencies: Vec<DependencyNode>,
    pub parsed_params: Vec<ParsedParameter>,
    pub is_async: bool,
    pub is_fast_path: bool,
}

#[must_use]
pub fn parse_route_metadata(py: Python, func: &Bound<PyAny>, path: &str) -> ParsedRouteMetadata {
    let response_type = get_response_type(py, func);
    let serialization_hint = get_serialization_hint(py, func);

    let is_async = func
        .getattr("__code__")
        .and_then(|code| code.getattr("co_flags"))
        .and_then(|flags| flags.extract::<u32>())
        .is_ok_and(|f| (f & 0x80) != 0);

    let path_param_names = fastrapi_core::path::extract_path_param_names(path);
    let dependencies =
        dependencies::parse_dependencies(py, func, &path_param_names).unwrap_or_default();

    let dep_param_names: HashSet<String> = dependencies
        .iter()
        .filter_map(|d| d.param_name.as_ref().map(|n| n.bind(py).to_string()))
        .collect();

    let mut param_validators = Vec::new();
    let mut body_param_names = Vec::new();
    let mut parsed_params = Vec::new();
    let mut request_param: Option<Py<PyString>> = None;
    let mut response_param = None;

    let request_cls: Py<PyAny> = py.get_type::<PyRequest>().into_any().unbind();

    _ = (|| -> PyResult<()> {
        let inspect = INSPECT.get(py)?;
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
                if dependencies::is_response_annotation(py, ann) {
                    response_param = Some(PyString::new(py, &param_name).unbind());
                    continue;
                }
                let ann_ref: &Bound<'_, PyAny> = ann;
                if ann_ref.is(request_cls.bind(py)) {
                    request_param = Some(PyString::new(py, &param_name).unbind());
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
                        .map(Bound::unbind);
                    let validate_json = ann_bound
                        .getattr(intern!(py, "model_validate_json"))
                        .ok()
                        .map(Bound::unbind);
                    let validate_python = ann_bound
                        .getattr(intern!(py, "model_validate"))
                        .map_or_else(|_| ann.clone_ref(py), Bound::unbind);
                    let validate_json_method = core_validator.as_ref().and_then(|core| {
                        core.bind(py)
                            .getattr(intern!(py, "validate_json"))
                            .ok()
                            .map(Bound::unbind)
                    });
                    param_validators.push(PydanticValidator {
                        name: parsed_param.name.clone(),
                        model_class: ann.clone_ref(py),
                        kernel: OnceLock::new(),
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
        response_param,
        dependencies,
        parsed_params,
        is_async,
        is_fast_path,
    }
}

#[must_use]
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

#[must_use]
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
            .or_else(|_| ann.str().map(Bound::into_any))?;
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

#[must_use]
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
            .or_else(|_| ann.str().map(Bound::into_any))?;
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
