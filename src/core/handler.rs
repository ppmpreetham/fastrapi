use crate::python::dependencies::DependencyNode;
use pyo3::prelude::*;

pub use crate::bridge::call_python::{
    convert_response_by_type, run_py_handler_no_args, run_py_handler_with_args,
    run_py_handler_with_params,
};

#[derive(Clone)]
pub struct RouteHandler {
    pub func: Py<PyAny>,
    pub is_async: bool,
    pub is_fast_path: bool,
    pub param_validators: Vec<(String, Py<PyAny>)>,
    pub response_type: crate::ResponseType,
    pub needs_kwargs: bool,
    pub path_param_names: Vec<String>,
    pub query_param_names: Vec<String>,
    pub body_param_names: Vec<String>,
    pub dependencies: Vec<DependencyNode>,
}
