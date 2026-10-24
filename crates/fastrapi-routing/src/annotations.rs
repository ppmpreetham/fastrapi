//! Helpers that examine Python type annotations.
//! The functions find scalar kinds (`bool`, `int`, `float`, `str`), get the members of a union, and remove `Optional`.
//! The request code uses the results to convert raw request values.

use crate::params;
use fastrapi_pyutil::imports::TYPING;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBool, PyFloat, PyInt, PyModule, PyString, PyTuple, PyType};
use pyo3::{Bound, Py, intern};
use smallvec::SmallVec;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScalarKind {
    Bool,
    Int,
    Float,
    Str,
    #[default]
    Other,
}

#[must_use]
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

#[must_use]
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

pub fn union_args<'py>(
    py: Python<'py>,
    annotation: &Bound<'py, PyAny>,
) -> Option<Bound<'py, PyTuple>> {
    let origin = TYPING
        .get(py)
        .ok()?
        .getattr(intern!(py, "get_origin"))
        .ok()?
        .call1((annotation,))
        .ok()?;
    if origin.is_none() {
        return None;
    }
    let is_union = TYPING
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

#[must_use]
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
