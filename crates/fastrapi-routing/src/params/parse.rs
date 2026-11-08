//! Converts one parameter of an endpoint signature into a `ParsedParameter`.
//! It finds the source of the value (path, query, header, body, or other source), the default value, the external name, and the constraints.

use super::constraints::extract_constraints;
use super::module::analyze;
use super::utils::{annotation_name, base_annotation, is_background_tasks_type, is_inspect_empty};
use crate::kernel_params::ParamKernel;
use crate::route::{EmptyDefault, FastKind, ModelKey, ParsedParameter};
use fastrapi_core::params::{ParameterConstraints, ParameterSource};
use fastrapi_http::upload_file::PyUploadFile;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyString, PyTuple};
use pyo3::{Bound, intern};
use std::sync::Arc;

fn source_of(name: &str) -> ParameterSource {
    match name {
        "path" => ParameterSource::Path,
        "query" => ParameterSource::Query,
        "header" => ParameterSource::Header,
        "cookie" => ParameterSource::Cookie,
        _ => ParameterSource::Body,
    }
}

fn empty_default(name: Option<&str>) -> Option<EmptyDefault> {
    match name? {
        "list" => Some(EmptyDefault::List),
        "dict" => Some(EmptyDefault::Dict),
        "set" => Some(EmptyDefault::Set),
        _ => None,
    }
}

fn fast_kind(name: Option<&str>) -> Option<FastKind> {
    match name? {
        "str" => Some(FastKind::Str),
        "int" => Some(FastKind::Int),
        "float" => Some(FastKind::Float),
        "bool" => Some(FastKind::Bool),
        _ => None,
    }
}

fn optional<'py>(
    spec: &Bound<'py, PyAny>,
    name: &Bound<'py, PyString>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    Ok(Some(spec.getattr(name)?).filter(|value| !value.is_none()))
}

fn model_keys(spec: &Bound<'_, PyAny>) -> PyResult<Option<Arc<[ModelKey]>>> {
    let py = spec.py();
    let Some(keys) = optional(spec, intern!(py, "model_keys"))? else {
        return Ok(None);
    };
    keys.try_iter()?
        .map(|key| {
            let key = key?;
            Ok(ModelKey {
                lookup: key.getattr(intern!(py, "lookup"))?.extract()?,
                alias: PyString::intern(py, key.getattr(intern!(py, "alias"))?.extract()?).unbind(),
                sequence: key.getattr(intern!(py, "sequence"))?.extract()?,
            })
        })
        .collect::<PyResult<Arc<[ModelKey]>>>()
        .map(Some)
}

fn background_tasks_parameter(py: Python<'_>, param_name: &str) -> ParsedParameter {
    let name_py = PyString::intern(py, param_name).unbind();
    ParsedParameter {
        name: param_name.to_owned(),
        alias_py: name_py.clone_ref(py),
        name_py,
        external_name: param_name.to_owned(),
        header_name: None,
        source: ParameterSource::BackgroundTasks,
        annotation: None,
        default_value: None,
        make_default: None,
        has_default: false,
        required: false,
        is_list: false,
        description: None,
        constraints: ParameterConstraints::default(),
        param_object: None,
        is_pydantic_model: false,
        model_keys: None,
        loc_renames: None,
        is_file: false,
        is_form: false,
        read_bytes: false,
        embed: false,
        fast: None,
        fast_list: None,
        fast_pattern: None,
        empty_default: None,
        kernel: None,
        validate: py.None(),
        validate_json: None,
        validator_index: None,
    }
}

pub fn parse_parameter_spec(
    py: Python<'_>,
    param_name: &str,
    param_obj: &Bound<'_, PyAny>,
    path_param_names: &[String],
) -> PyResult<ParsedParameter> {
    let raw_annotation = param_obj.getattr(intern!(py, "annotation"))?.into_any();
    let has_annotation = !is_inspect_empty(py, &raw_annotation);
    if has_annotation
        && annotation_name(py, &base_annotation(&raw_annotation).unbind())
            .as_deref()
            .is_some_and(is_background_tasks_type)
    {
        return Ok(background_tasks_parameter(py, param_name));
    }

    let is_path_param = path_param_names.iter().any(|name| name == param_name);
    let uploads = PyTuple::new(py, [py.get_type::<PyUploadFile>()])?;
    let spec = analyze(py)?.call1((param_name, param_obj, is_path_param, uploads))?;

    let source = source_of(spec.getattr(intern!(py, "source"))?.extract::<&str>()?);
    let external_name: String = spec.getattr(intern!(py, "alias"))?.extract()?;
    let field_info = spec.getattr(intern!(py, "field_info"))?;
    let required: bool = spec.getattr(intern!(py, "required"))?.extract()?;
    let header_name = matches!(source, ParameterSource::Header)
        .then(|| {
            axum::http::HeaderName::from_bytes(external_name.to_ascii_lowercase().as_bytes()).ok()
        })
        .flatten();
    let constraints = extract_constraints(&spec.getattr(intern!(py, "constraints"))?);
    let fast_pattern = constraints
        .pattern
        .as_deref()
        .map(regex::Regex::new)
        .transpose();
    let fast = fast_kind(
        spec.getattr(intern!(py, "fast"))?
            .extract::<Option<&str>>()?,
    )
    .filter(|_| fast_pattern.is_ok());
    let fast_list = fast_kind(
        spec.getattr(intern!(py, "fast_list"))?
            .extract::<Option<&str>>()?,
    );
    let model_keys = model_keys(&spec)?;
    let is_model: bool = spec.getattr(intern!(py, "model"))?.extract()?;
    let is_file: bool = spec.getattr(intern!(py, "is_file"))?.extract()?;
    let kernel = (!is_model && !is_file)
        .then(|| ParamKernel::compile(&spec.getattr(intern!(py, "core_schema")).ok()?))
        .flatten()
        .map(Arc::new);

    Ok(ParsedParameter {
        name: param_name.to_owned(),
        name_py: PyString::intern(py, param_name).unbind(),
        alias_py: PyString::intern(py, &external_name).unbind(),
        header_name,
        source,
        annotation: has_annotation
            .then(|| spec.getattr(intern!(py, "annotation")))
            .transpose()?
            .map(Bound::unbind),
        default_value: optional(&spec, intern!(py, "default"))?.map(Bound::unbind),
        make_default: optional(&spec, intern!(py, "make_default"))?.map(Bound::unbind),
        has_default: !required,
        required,
        is_list: spec.getattr(intern!(py, "sequence"))?.extract()?,
        description: field_info
            .getattr(intern!(py, "description"))?
            .extract::<Option<String>>()?,
        constraints,
        is_pydantic_model: is_model,
        model_keys,
        loc_renames: optional(&spec, intern!(py, "loc_renames"))?.map(Bound::unbind),
        is_file,
        is_form: spec.getattr(intern!(py, "is_form"))?.extract()?,
        read_bytes: spec.getattr(intern!(py, "read_bytes"))?.extract()?,
        embed: spec.getattr(intern!(py, "embed"))?.extract()?,
        fast,
        fast_list,
        fast_pattern: fast_pattern.ok().flatten(),
        empty_default: empty_default(
            spec.getattr(intern!(py, "empty_default"))?
                .extract::<Option<&str>>()?,
        ),
        kernel,
        validate: spec.getattr(intern!(py, "validate"))?.unbind(),
        validate_json: optional(&spec, intern!(py, "validate_json"))?.map(Bound::unbind),
        param_object: Some(field_info.unbind()),
        external_name,
        validator_index: None,
    })
}
