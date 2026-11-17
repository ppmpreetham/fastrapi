//! Helpers that examine parameter annotations and markers.
//! The functions find `Annotated` markers, the base type of an annotation, and the element type of a list.

use crate::annotations::strip_optional;
use crate::params::PyDepends;
use crate::params::module::params_module;
use crate::security::PySecurityScopes;
use fastrapi_core::params::ParameterSource;
use pyo3::exceptions::PyAssertionError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyTuple, PyType};

fastrapi_pyutil::cached_py_import!(
    STARLETTE_BACKGROUND_TASKS,
    "starlette.background",
    "BackgroundTasks"
);
fastrapi_pyutil::cached_py_import!(PYDANTIC_FIELD_INFO, "pydantic.fields", "FieldInfo");
fastrapi_pyutil::cached_py_import!(INSPECT_PARAMETER_EMPTY, "inspect", "Parameter", "empty");

pub fn is_inspect_empty(py: Python<'_>, value: &Bound<'_, PyAny>) -> bool {
    INSPECT_PARAMETER_EMPTY.is(py, value)
}

/// The injected parameter source of a `BackgroundTasks` or `SecurityScopes` annotation, matched by class.
#[must_use]
pub fn injected_source(py: Python<'_>, annotation: &Bound<'_, PyAny>) -> Option<ParameterSource> {
    let class = base_annotation(annotation).cast_into::<PyType>().ok()?;
    if class.is_subclass_of::<PySecurityScopes>().unwrap_or(false) {
        return Some(ParameterSource::SecurityScopes);
    }
    STARLETTE_BACKGROUND_TASKS
        .get(py)
        .is_ok_and(|base| class.is_subclass(base).unwrap_or(false))
        .then_some(ParameterSource::BackgroundTasks)
}

#[must_use]
pub fn base_annotation<'py>(annotation: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    if annotation.hasattr("__metadata__").unwrap_or(false)
        && let Ok(args) = annotation.getattr("__args__")
        && let Ok(first) = args.get_item(0)
    {
        return first;
    }
    annotation.clone()
}

#[must_use]
pub fn list_element_annotation<'py>(
    py: Python<'py>,
    annotation: &Bound<'py, PyAny>,
) -> (bool, Option<Bound<'py, PyAny>>) {
    let annotation = strip_optional(py, annotation).unwrap_or_else(|| annotation.clone());
    let is_type_named = |obj: &Bound<'py, PyAny>, name: &str| {
        obj.get_type()
            .name()
            .is_ok_and(|n| n.to_string_lossy() == name)
            || obj
                .getattr("__name__")
                .ok()
                .and_then(|n| n.extract::<String>().ok())
                .as_deref()
                == Some(name)
    };

    if let Ok(origin) = annotation.getattr("__origin__")
        && (is_type_named(&origin, "list") || is_type_named(&origin, "set"))
        && let Ok(args) = annotation.getattr("__args__")
        && let Ok(elem) = args.get_item(0)
    {
        return (true, Some(elem));
    }

    if is_type_named(&annotation, "list") || is_type_named(&annotation, "set") {
        return (true, None);
    }

    (false, None)
}

/// Fails like FastAPI when a FastAPI annotation such as `Query()` is attached to an injected type.
pub(crate) fn assert_no_field_info(py: Python<'_>, annotation: &Bound<'_, PyAny>) -> PyResult<()> {
    let Ok(metadata) = annotation.getattr(intern!(py, "__metadata__")) else {
        return Ok(());
    };
    let field_info = PYDANTIC_FIELD_INFO.get(py)?;
    for item in metadata.try_iter()? {
        if item?.is_instance(field_info)? {
            return Err(PyAssertionError::new_err(format!(
                "Cannot specify FastAPI annotation for type {}",
                base_annotation(annotation).repr()?
            )));
        }
    }
    Ok(())
}

#[inline]
pub(crate) fn is_dependency_marker(marker: &Bound<'_, PyAny>) -> bool {
    let py = marker.py();
    // FastAPI's own `Depends` is a different class, so it is recognised by its two fields.
    marker.is_instance_of::<PyDepends>()
        || (marker.hasattr(intern!(py, "dependency")).unwrap_or(false)
            && marker.hasattr(intern!(py, "use_cache")).unwrap_or(false))
}

/// The `Depends` that wins in `Annotated`: like FastAPI, the last of its `Depends`, `Param` and `Body` markers.
#[must_use]
pub fn find_annotated_dependency_marker<'py>(
    param_obj: &Bound<'py, PyAny>,
) -> Option<Bound<'py, PyAny>> {
    let py = param_obj.py();
    let metadata = param_obj
        .getattr("annotation")
        .ok()?
        .getattr("__metadata__")
        .ok()?;
    let params = params_module(py).ok()?;
    let field_markers = PyTuple::new(
        py,
        [params.getattr("Param").ok()?, params.getattr("Body").ok()?],
    )
    .ok()?;
    let winner = metadata
        .try_iter()
        .ok()?
        .flatten()
        .filter(|item| {
            is_dependency_marker(item) || item.is_instance(&field_markers).unwrap_or(false)
        })
        .last()?;
    is_dependency_marker(&winner).then_some(winner)
}
