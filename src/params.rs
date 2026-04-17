use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use std::sync::Arc;

use crate::types::route::{ParameterConstraints, ParameterSource, ParsedParameter};

// utils

/// from route patterns like "/users/{user_id}"
pub fn extract_path_param_names(path: &str) -> Vec<String> {
    let mut params = Vec::new();
    let mut in_param = false;
    let mut current_param = String::new();
    for c in path.chars() {
        match c {
            '{' => {
                in_param = true;
                current_param.clear();
            }
            '}' => {
                if in_param && !current_param.is_empty() {
                    params.push(current_param.clone());
                }
                in_param = false;
            }
            _ => {
                if in_param {
                    current_param.push(c);
                }
            }
        }
    }
    params
}

pub fn is_inspect_empty(py: Python<'_>, value: &Bound<'_, PyAny>) -> bool {
    py.import("inspect")
        .ok()
        .and_then(|inspect| inspect.getattr("Parameter").ok())
        .and_then(|parameter| parameter.getattr("empty").ok())
        .map(|empty| value.is(&empty))
        .unwrap_or(false)
}

fn is_ellipsis(value: &Bound<'_, PyAny>) -> bool {
    value
        .get_type()
        .name()
        .map(|name| name == "ellipsis")
        .unwrap_or(false)
}

fn extract_constraints(param_obj: &Bound<'_, PyAny>) -> ParameterConstraints {
    fn extract_opt<T: for<'a, 'py> FromPyObject<'a, 'py>>(
        obj: &Bound<'_, PyAny>,
        attr: &str,
    ) -> Option<T> {
        obj.getattr(attr)
            .ok()
            .and_then(|value| value.extract::<T>().ok())
    }

    let pattern = param_obj
        .getattr("pattern")
        .ok()
        .and_then(|value| value.extract::<Option<String>>().ok())
        .flatten()
        .and_then(|pattern| regex::Regex::new(&format!("^(?:{pattern})$")).ok())
        .map(Arc::new);

    ParameterConstraints {
        gt: extract_opt(param_obj, "gt"),
        ge: extract_opt(param_obj, "ge"),
        lt: extract_opt(param_obj, "lt"),
        le: extract_opt(param_obj, "le"),
        min_length: extract_opt(param_obj, "min_length"),
        max_length: extract_opt(param_obj, "max_length"),
        pattern,
    }
}

fn source_from_param_class(type_name: &str) -> Option<ParameterSource> {
    match type_name {
        "Query" => Some(ParameterSource::Query),
        "Path" => Some(ParameterSource::Path),
        "Body" | "Form" | "File" => Some(ParameterSource::Body),
        "Header" => Some(ParameterSource::Header),
        "Cookie" => Some(ParameterSource::Cookie),
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
    if let Ok(alias) = param_obj.getattr("alias") {
        if let Ok(Some(alias)) = alias.extract::<Option<String>>() {
            return alias;
        }
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

pub fn parse_parameter_spec(
    py: Python<'_>,
    param_name: &str,
    param_obj: &Bound<'_, PyAny>,
    path_param_names: &[String],
) -> PyResult<ParsedParameter> {
    let annotation = param_obj
        .getattr("annotation")
        .ok()
        .filter(|annotation| !is_inspect_empty(py, annotation))
        .map(|annotation| annotation.unbind());

    let is_pydantic_model = annotation
        .as_ref()
        .map(|annotation| crate::pydantic::is_pydantic_model(py, annotation.bind(py)))
        .unwrap_or(false);

    let default = param_obj.getattr("default")?;
    let mut source = if path_param_names.iter().any(|name| name == param_name) {
        ParameterSource::Path
    } else if is_pydantic_model {
        ParameterSource::Body
    } else {
        ParameterSource::Query
    };

    let mut default_value = None;
    let mut has_default = false;
    let mut required = !path_param_names.iter().any(|name| name == param_name);
    let mut description = None;
    let mut constraints = ParameterConstraints::default();
    let mut param_object = None;

    if !is_inspect_empty(py, &default) {
        let type_name = default.get_type().name()?.to_string();
        if let Some(param_source) = source_from_param_class(&type_name) {
            source = param_source;
            let (value, has_value, is_required) = extract_param_default(&default);
            default_value = value;
            has_default = has_value;
            required = is_required;
            description = default
                .getattr("description")
                .ok()
                .and_then(|value| value.extract::<Option<String>>().ok())
                .flatten();
            constraints = extract_constraints(&default);
            param_object = Some(default.unbind());
        } else {
            default_value = Some(default.unbind());
            has_default = true;
            required = false;
        }
    } else if matches!(source, ParameterSource::Path) {
        required = true;
    }

    let external_name = if let Some(param_object) = &param_object {
        external_name_for_param(param_name, &source, param_object.bind(py))
    } else {
        param_name.to_string()
    };

    Ok(ParsedParameter {
        name: param_name.to_string(),
        external_name,
        source,
        annotation,
        default_value,
        has_default,
        required,
        description,
        constraints,
        param_object,
        is_pydantic_model,
        scalar_kind: crate::pydantic::ScalarKind::Other,
    })
}

// sentinels

#[pyclass(name = "Unset", skip_from_py_object)]
#[derive(Clone)]
pub struct Unset;

#[pymethods]
impl Unset {
    #[new]
    fn new() -> Self {
        Self
    }
}

#[pyclass(name = "Undefined", skip_from_py_object)]
#[derive(Clone)]
pub struct Undefined;

#[pymethods]
impl Undefined {
    #[new]
    fn new() -> Self {
        Self
    }
}

// dependency classes

#[pyclass(name = "Depends", subclass, skip_from_py_object)]
#[derive(Clone)]
pub struct PyDepends {
    #[pyo3(get)]
    pub dependency: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub use_cache: bool,
}

#[pymethods]
impl PyDepends {
    #[new]
    #[pyo3(signature = (dependency=None, *, use_cache=true))]
    pub fn new(dependency: Option<Py<PyAny>>, use_cache: bool) -> Self {
        Self {
            dependency,
            use_cache,
        }
    }
}

#[pyclass(name = "Security", extends = PyDepends, skip_from_py_object)]
#[derive(Clone)]
pub struct PySecurity {
    #[pyo3(get)]
    pub scopes: Vec<String>,
}

#[pymethods]
impl PySecurity {
    #[new]
    #[pyo3(signature = (dependency=None, *, scopes=None, use_cache=true))]
    fn new(
        dependency: Option<Py<PyAny>>,
        scopes: Option<Vec<String>>,
        use_cache: bool,
    ) -> (Self, PyDepends) {
        (
            Self {
                scopes: scopes.unwrap_or_default(),
            },
            PyDepends::new(dependency, use_cache),
        )
    }
}

// macros

/// Generates a standard param class (Query, Path, Cookie) with the shared field set.
macro_rules! define_param {
    // Query, Path, Cookie
    (base: $struct_name:ident, $py_name:literal) => {
        #[pyclass(name = $py_name, skip_from_py_object)]
        #[derive(Clone)]
        pub struct $struct_name {
            #[pyo3(get)] pub default: Option<Py<PyAny>>,
            #[pyo3(get)] pub alias: Option<String>,
            #[pyo3(get)] pub title: Option<String>,
            #[pyo3(get)] pub description: Option<String>,
            #[pyo3(get)] pub gt: Option<f64>,
            #[pyo3(get)] pub ge: Option<f64>,
            #[pyo3(get)] pub lt: Option<f64>,
            #[pyo3(get)] pub le: Option<f64>,
            #[pyo3(get)] pub min_length: Option<usize>,
            #[pyo3(get)] pub max_length: Option<usize>,
            #[pyo3(get)] pub pattern: Option<String>,
            #[pyo3(get)] pub deprecated: Option<bool>,
            #[pyo3(get)] pub include_in_schema: bool,
            #[pyo3(get)] pub examples: Option<Py<PyAny>>,
        }
        #[pymethods]
        impl $struct_name {
            #[new]
            #[pyo3(signature = (default=None, *, alias=None, title=None, description=None, gt=None, ge=None, lt=None, le=None, min_length=None, max_length=None, pattern=None, deprecated=None, include_in_schema=true, examples=None, **_extra))]
            #[allow(clippy::too_many_arguments)]
            fn new(
                default: Option<Py<PyAny>>,
                alias: Option<String>,
                title: Option<String>,
                description: Option<String>,
                gt: Option<f64>, ge: Option<f64>, lt: Option<f64>, le: Option<f64>,
                min_length: Option<usize>, max_length: Option<usize>,
                pattern: Option<String>, deprecated: Option<bool>,
                include_in_schema: bool, examples: Option<Py<PyAny>>,
                _extra: Option<&Bound<'_, PyDict>>,
            ) -> Self {
                Self {
                    default, alias, title, description,
                    gt, ge, lt, le, min_length, max_length,
                    pattern, deprecated, include_in_schema, examples,
                }
            }
        }
    };

    // Header
    (header: $struct_name:ident, $py_name:literal) => {
        #[pyclass(name = $py_name, skip_from_py_object)]
        #[derive(Clone)]
        pub struct $struct_name {
            #[pyo3(get)] pub default: Option<Py<PyAny>>,
            #[pyo3(get)] pub alias: Option<String>,
            #[pyo3(get)] pub convert_underscores: bool,
            #[pyo3(get)] pub title: Option<String>,
            #[pyo3(get)] pub description: Option<String>,
            #[pyo3(get)] pub gt: Option<f64>,
            #[pyo3(get)] pub ge: Option<f64>,
            #[pyo3(get)] pub lt: Option<f64>,
            #[pyo3(get)] pub le: Option<f64>,
            #[pyo3(get)] pub min_length: Option<usize>,
            #[pyo3(get)] pub max_length: Option<usize>,
            #[pyo3(get)] pub pattern: Option<String>,
            #[pyo3(get)] pub deprecated: Option<bool>,
            #[pyo3(get)] pub include_in_schema: bool,
            #[pyo3(get)] pub examples: Option<Py<PyAny>>,
        }
        #[pymethods]
        impl $struct_name {
            #[new]
            #[pyo3(signature = (default=None, *, alias=None, convert_underscores=true, title=None, description=None, gt=None, ge=None, lt=None, le=None, min_length=None, max_length=None, pattern=None, deprecated=None, include_in_schema=true, examples=None, **_extra))]
            #[allow(clippy::too_many_arguments)]
            fn new(
                default: Option<Py<PyAny>>,
                alias: Option<String>,
                convert_underscores: bool,
                title: Option<String>,
                description: Option<String>,
                gt: Option<f64>, ge: Option<f64>, lt: Option<f64>, le: Option<f64>,
                min_length: Option<usize>, max_length: Option<usize>,
                pattern: Option<String>, deprecated: Option<bool>,
                include_in_schema: bool, examples: Option<Py<PyAny>>,
                _extra: Option<&Bound<'_, PyDict>>,
            ) -> Self {
                Self {
                    default, alias, convert_underscores, title, description,
                    gt, ge, lt, le, min_length, max_length,
                    pattern, deprecated, include_in_schema, examples,
                }
            }
        }
    };

    // Body (embed + media_type), Form, File (media_type only)
    (media: $struct_name:ident, $py_name:literal, $sig:tt,
     ctor_head: { $($ctor_head_name:ident : $ctor_head_ty:ty),* },
     extra_fields: { $($extra_field:ident : $extra_fty:ty),* },
     self_head: { $($self_head:ident),* }
    ) => {
        #[pyclass(name = $py_name, skip_from_py_object)]
        #[derive(Clone)]
        pub struct $struct_name {
            #[pyo3(get)] pub default: Option<Py<PyAny>>,
            $(#[pyo3(get)] pub $extra_field: $extra_fty,)*
            #[pyo3(get)] pub alias: Option<String>,
            #[pyo3(get)] pub title: Option<String>,
            #[pyo3(get)] pub description: Option<String>,
            #[pyo3(get)] pub gt: Option<f64>,
            #[pyo3(get)] pub ge: Option<f64>,
            #[pyo3(get)] pub lt: Option<f64>,
            #[pyo3(get)] pub le: Option<f64>,
            #[pyo3(get)] pub min_length: Option<usize>,
            #[pyo3(get)] pub max_length: Option<usize>,
            #[pyo3(get)] pub pattern: Option<String>,
            #[pyo3(get)] pub deprecated: Option<bool>,
            #[pyo3(get)] pub include_in_schema: bool,
            #[pyo3(get)] pub examples: Option<Py<PyAny>>,
        }
        #[pymethods]
        impl $struct_name {
            #[new]
            #[pyo3(signature = $sig)]
            #[allow(clippy::too_many_arguments)]
            fn new(
                default: Option<Py<PyAny>>,
                $($ctor_head_name: $ctor_head_ty,)*
                alias: Option<String>,
                title: Option<String>,
                description: Option<String>,
                gt: Option<f64>, ge: Option<f64>, lt: Option<f64>, le: Option<f64>,
                min_length: Option<usize>, max_length: Option<usize>,
                pattern: Option<String>, deprecated: Option<bool>,
                include_in_schema: bool, examples: Option<Py<PyAny>>,
                _extra: Option<&Bound<'_, PyDict>>,
            ) -> Self {
                Self {
                    default,
                    $($self_head,)*
                    alias, title, description,
                    gt, ge, lt, le, min_length, max_length,
                    pattern, deprecated, include_in_schema, examples,
                }
            }
        }
    };
}

// parameter classes

// Query
define_param!(base: PyQuery, "Query");

// Path
define_param!(base: PyPath, "Path");

// Cookie
define_param!(base: PyCookie, "Cookie");

// Header
define_param!(header: PyHeader, "Header");

// Body (embed, media_type)
define_param!(media: PyBody, "Body",
    (default=None, *, embed=None, media_type="application/json".to_string(), alias=None, title=None, description=None, gt=None, ge=None, lt=None, le=None, min_length=None, max_length=None, pattern=None, deprecated=None, include_in_schema=true, examples=None, **_extra),
    ctor_head: { embed: Option<bool>, media_type: String },
    extra_fields: { embed: Option<bool>, media_type: String },
    self_head: { embed, media_type }
);

// Form (media_type)
define_param!(media: PyForm, "Form",
    (default=None, *, media_type="application/x-www-form-urlencoded".to_string(), alias=None, title=None, description=None, gt=None, ge=None, lt=None, le=None, min_length=None, max_length=None, pattern=None, deprecated=None, include_in_schema=true, examples=None, **_extra),
    ctor_head: { media_type: String },
    extra_fields: { media_type: String },
    self_head: { media_type }
);

// File
define_param!(media: PyFile, "File",
    (default=None, *, media_type="multipart/form-data".to_string(), alias=None, title=None, description=None, gt=None, ge=None, lt=None, le=None, min_length=None, max_length=None, pattern=None, deprecated=None, include_in_schema=true, examples=None, **_extra),
    ctor_head: { media_type: String },
    extra_fields: { media_type: String },
    self_head: { media_type }
);
