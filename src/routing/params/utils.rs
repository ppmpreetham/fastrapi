use pyo3::prelude::*;
use pyo3::types::{PyAny, PyString};

crate::cached_py_import!(INSPECT_PARAMETER_EMPTY, "inspect", "Parameter", "empty");

pub fn is_inspect_empty(py: Python<'_>, value: &Bound<'_, PyAny>) -> bool {
    INSPECT_PARAMETER_EMPTY.is(py, value)
}

pub fn is_ellipsis(value: &Bound<'_, PyAny>) -> bool {
    value
        .get_type()
        .name()
        .map(|name| name == "ellipsis")
        .unwrap_or(false)
}

pub fn annotation_name(py: Python<'_>, annotation: &Py<PyAny>) -> Option<String> {
    let annotation = annotation.bind(py);
    annotation
        .getattr("__name__")
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
}

#[inline]
pub fn is_upload_file_type(name: &str) -> bool {
    name == "UploadFile" || name.ends_with(".UploadFile")
}

#[inline]
pub fn is_background_tasks_type(name: &str) -> bool {
    name == "BackgroundTasks" || name.ends_with(".BackgroundTasks")
}

pub fn base_annotation<'py>(_py: Python<'py>, annotation: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    if annotation.hasattr("__metadata__").unwrap_or(false)
        && let Ok(args) = annotation.getattr("__args__")
        && let Ok(first) = args.get_item(0)
    {
        return first;
    }
    annotation.clone()
}

pub fn list_element_annotation<'py>(
    py: Python<'py>,
    annotation: &Bound<'py, PyAny>,
) -> (bool, Option<Bound<'py, PyAny>>) {
    // `Optional[List[int]]` and `Union[List[int], None]` behave as list params.
    let annotation =
        crate::ffi::pydantic::strip_optional(py, annotation).unwrap_or_else(|| annotation.clone());
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

#[inline]
pub(crate) fn is_param_marker(marker: &Bound<'_, PyAny>) -> bool {
    let name = marker
        .get_type()
        .name()
        .ok()
        .map(|n| n.to_string_lossy().into_owned());
    matches!(
        name.as_deref(),
        Some("Query")
            | Some("Path")
            | Some("Body")
            | Some("Form")
            | Some("File")
            | Some("Header")
            | Some("Cookie")
    )
}
#[inline]
pub(crate) fn is_dependency_marker(marker: &Bound<'_, PyAny>) -> bool {
    marker.hasattr("dependency").unwrap_or(false) || marker.hasattr("scopes").unwrap_or(false)
}

pub fn find_annotated_marker<'py>(param_obj: &Bound<'py, PyAny>) -> Option<Bound<'py, PyAny>> {
    let annotation = param_obj.getattr("annotation").ok()?;
    let metadata = annotation.getattr("__metadata__").ok()?;

    for item in metadata.try_iter().ok()? {
        let Ok(item) = item else { continue };
        if is_dependency_marker(&item) || is_param_marker(&item) {
            return Some(item);
        }
    }
    None
}

pub fn find_annotated_dependency_marker<'py>(
    param_obj: &Bound<'py, PyAny>,
) -> Option<Bound<'py, PyAny>> {
    let annotation = param_obj.getattr("annotation").ok()?;
    let metadata = annotation.getattr("__metadata__").ok()?;

    for item in metadata.try_iter().ok()? {
        let Ok(item) = item else { continue };
        if is_dependency_marker(&item) {
            return Some(item);
        }
    }
    None
}
