use super::super::types::{ParameterConstraints, ParameterSource, ParsedParameter};
use super::constraints::extract_constraints;
use super::utils::{
    annotation_name, base_annotation, find_annotated_marker, is_background_tasks_type,
    is_dependency_marker, is_ellipsis, is_inspect_empty, is_upload_file_type,
    list_element_annotation,
};
use crate::ffi::pydantic;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyString};

pub const PARAM_CLASS_QUERY: &str = "Query";
pub const PARAM_CLASS_PATH: &str = "Path";
pub const PARAM_CLASS_BODY: &str = "Body";
pub const PARAM_CLASS_FORM: &str = "Form";
pub const PARAM_CLASS_FILE: &str = "File";
pub const PARAM_CLASS_HEADER: &str = "Header";
pub const PARAM_CLASS_COOKIE: &str = "Cookie";

fn source_from_param_class(type_name: &str) -> Option<ParameterSource> {
    match type_name {
        PARAM_CLASS_QUERY => Some(ParameterSource::Query),
        PARAM_CLASS_PATH => Some(ParameterSource::Path),
        PARAM_CLASS_BODY | PARAM_CLASS_FORM | PARAM_CLASS_FILE => Some(ParameterSource::Body),
        PARAM_CLASS_HEADER => Some(ParameterSource::Header),
        PARAM_CLASS_COOKIE => Some(ParameterSource::Cookie),
        _ => None,
    }
}

fn extract_param_default(param_obj: &Bound<'_, PyAny>) -> (Option<Py<PyAny>>, bool, bool) {
    let Ok(default) = param_obj.getattr("default") else {
        return (None, false, true);
    };
    if is_ellipsis(&default) {
        return (None, false, true);
    }
    if default.is_none() {
        return (None, true, false);
    }
    (Some(default.unbind()), true, false)
}

fn external_name_for_param(
    param_name: &str,
    source: &ParameterSource,
    param_obj: &Bound<'_, PyAny>,
) -> String {
    if let Ok(alias) = param_obj.getattr("alias")
        && let Ok(Some(alias)) = alias.extract::<Option<String>>()
    {
        return alias;
    }
    if matches!(source, ParameterSource::Header)
        && param_obj
            .getattr("convert_underscores")
            .ok()
            .and_then(|value| value.extract::<bool>().ok())
            .unwrap_or(true)
    {
        return param_name.replace('_', "-");
    }
    param_name.to_string()
}

#[inline]
fn type_name_of(value: &Bound<'_, PyAny>) -> String {
    value
        .get_type()
        .name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn parse_parameter_spec(
    py: Python<'_>,
    param_name: &str,
    param_obj: &Bound<'_, PyAny>,
    path_param_names: &[String],
) -> PyResult<ParsedParameter> {
    let raw_annotation = param_obj
        .getattr("annotation")
        .ok()
        .filter(|annotation| !is_inspect_empty(py, annotation));
    let has_annotation = raw_annotation.is_some();
    let annotation = raw_annotation.map(|raw| base_annotation(py, &raw).unbind());
    let (is_list, element_annotation) = annotation
        .as_ref()
        .map(|ann| {
            let (is_list, elem) = list_element_annotation(py, ann.bind(py));
            (is_list, elem.map(|e| e.unbind()))
        })
        .unwrap_or((false, None));

    let effective_annotation = if is_list {
        element_annotation.or_else(|| annotation.clone())
    } else {
        annotation.clone()
    };

    let is_pydantic_model = effective_annotation
        .as_ref()
        .is_some_and(|ann| pydantic::is_pydantic_model(py, ann.bind(py)));

    let ann_name = effective_annotation
        .as_ref()
        .and_then(|ann| annotation_name(py, ann));
    let is_upload_file = ann_name.as_deref().is_some_and(is_upload_file_type);
    let is_background_tasks = ann_name.as_deref().is_some_and(is_background_tasks_type);

    let default = param_obj.getattr("default")?;
    let has_plain_default = !is_inspect_empty(py, &default);
    let is_path_param = path_param_names.iter().any(|name| name == param_name);

    let initial_source = if is_background_tasks {
        ParameterSource::BackgroundTasks
    } else if is_path_param {
        ParameterSource::Path
    } else if is_upload_file || is_pydantic_model {
        ParameterSource::Body
    } else if !has_annotation && !has_plain_default {
        // FastAPI rule: a required parameter with no annotation IS the body
        // (`def echo(data)`), not a query parameter.
        ParameterSource::Body
    } else {
        ParameterSource::Query
    };

    let plain_default_is_marker =
        has_plain_default && source_from_param_class(&type_name_of(&default)).is_some();

    let driver = if plain_default_is_marker {
        Some(default.clone())
    } else {
        find_annotated_marker(param_obj).filter(|marker| !is_dependency_marker(marker))
    };

    let (source, default_value, has_default, required, description, constraints, param_object) =
        if let Some(marker) = driver {
            let param_source =
                source_from_param_class(&type_name_of(&marker)).unwrap_or(initial_source);
            let (mut value, mut has_val, mut is_req) = extract_param_default(&marker);
            if has_plain_default && !plain_default_is_marker && !is_ellipsis(&default) {
                value = (!default.is_none()).then(|| default.clone().unbind());
                has_val = true;
                is_req = false;
            }
            let desc = marker
                .getattr("description")
                .ok()
                .and_then(|value| value.extract::<Option<String>>().ok())
                .flatten();
            let cons = extract_constraints(&marker);
            (
                param_source,
                value,
                has_val,
                is_req,
                desc,
                cons,
                Some(marker.unbind()),
            )
        } else if has_plain_default {
            (
                initial_source,
                Some(default.unbind()),
                true,
                false,
                None,
                ParameterConstraints::default(),
                None,
            )
        } else {
            let required = is_path_param
                || (matches!(initial_source, ParameterSource::Body) && !has_annotation);
            (
                initial_source,
                None,
                false,
                required,
                None,
                ParameterConstraints::default(),
                None,
            )
        };

    let external_name = param_object
        .as_ref()
        .map(|obj| external_name_for_param(param_name, &source, obj.bind(py)))
        .unwrap_or_else(|| param_name.to_string());

    let is_file_param = param_object
        .as_ref()
        .and_then(|obj| obj.bind(py).get_type().name().ok())
        .is_some_and(|name| name.to_string_lossy() == PARAM_CLASS_FILE);
    let is_file = is_upload_file || is_file_param;

    Ok(ParsedParameter {
        name: param_name.to_string(),
        name_py: PyString::new(py, param_name).unbind(),
        external_name,
        source,
        annotation,
        default_value,
        has_default,
        required,
        is_list,
        description,
        constraints,
        param_object,
        is_pydantic_model,
        is_file,
        scalar_kind: pydantic::ScalarKind::Other,
        validator_index: None,
    })
}
