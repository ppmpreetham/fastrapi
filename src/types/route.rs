use super::response::ResponseType;
use crate::dependencies::DependencyNode;
use pyo3::{Py, PyAny};

#[derive(Clone)]
pub struct RouteHandler {
    pub func: Py<PyAny>,
    pub is_async: bool,
    pub is_fast_path: bool,
    pub param_validators: Vec<(String, Py<PyAny>)>,
    pub response_type: ResponseType,
    pub needs_kwargs: bool,
    pub path_param_names: Vec<String>,
    pub query_param_names: Vec<String>,
    pub body_param_names: Vec<String>,
    pub dependencies: Vec<DependencyNode>,
}
