//! Helpers that examine Python type annotations.
//! The functions get the members of a union and remove `Optional`.

use crate::params;
use fastrapi_http::request::PyHTTPConnection;
use fastrapi_http::websocket::PyWebSocket;
use fastrapi_pyutil::imports::TYPING;
use fastrapi_pyutil::lazy::LazyPyAttr;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyModule, PyTuple, PyType};
use pyo3::{Bound, Py, intern};

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

#[must_use]
pub fn is_none_type(obj: &Bound<'_, PyAny>) -> bool {
    obj.is(obj.py().None().bind(obj.py()).get_type())
}

fastrapi_pyutil::cached_py_import!(STARLETTE_CONNECTION, "starlette.requests", "HTTPConnection");
fastrapi_pyutil::cached_py_import!(STARLETTE_WEBSOCKET, "starlette.websockets", "WebSocket");

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnectionKind {
    Request,
    WebSocket,
}

pub(crate) fn connection_kind(
    py: Python<'_>,
    annotation: &Bound<'_, PyAny>,
) -> Option<ConnectionKind> {
    let annotation = params::base_annotation(annotation);
    let class = annotation.cast::<PyType>().ok()?;
    let subclass_of = |base: &LazyPyAttr| {
        base.get(py)
            .is_ok_and(|base| class.is_subclass(base).unwrap_or(false))
    };
    if class.is_subclass_of::<PyWebSocket>().unwrap_or(false) || subclass_of(&STARLETTE_WEBSOCKET) {
        return Some(ConnectionKind::WebSocket);
    }
    (class.is_subclass_of::<PyHTTPConnection>().unwrap_or(false)
        || subclass_of(&STARLETTE_CONNECTION))
    .then_some(ConnectionKind::Request)
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
