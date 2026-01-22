use pyo3::prelude::*;
use pyo3::types::PyModule;

mod app;
mod background;
mod datastructures;
mod dependencies;
mod exceptions;
mod middlewares;
mod openapi;
mod params;
mod py_handlers;
mod pydantic;
mod request;
mod responses;
mod security;
mod server;
mod status;
mod utils;
mod websocket;

pub use app::FastrAPI;
pub use request::{PyHTTPConnection, PyRequest};
pub use responses::{PyHTMLResponse, PyJSONResponse, PyPlainTextResponse, PyRedirectResponse};

use crate::middlewares::PyMiddleware;
use once_cell::sync::Lazy;
use papaya::HashMap as PapayaHashMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseType {
    Json,
    Html,
    PlainText,
    Redirect,
    Auto,
}

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
    pub dependencies: Vec<crate::dependencies::DependencyInfo>,
}

pub static ROUTES: Lazy<PapayaHashMap<String, RouteHandler>> =
    Lazy::new(|| PapayaHashMap::with_capacity(128));

pub static MIDDLEWARES: Lazy<PapayaHashMap<String, Arc<PyMiddleware>>> =
    Lazy::new(|| PapayaHashMap::with_capacity(16));

#[pymodule(gil_used = false)]
fn fastrapi(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<FastrAPI>()?;
    m.add("FastrAPI", m.getattr("FastrAPI")?)?; // TODO: check if fastapi actually does have issue with this

    responses::register(m)?;
    exceptions::register(m)?;
    request::register(m)?;
    datastructures::register(m)?;
    background::register(m)?;
    security::register(m)?;
    pydantic::register_pydantic_integration(m)?;
    status::create_status_submodule(m)?;
    params::register(m)?;
    middlewares::register(m)?;
    websocket::register(m)?;

    // m.add("__version__", "0.1.0")?;
    Ok(())
}
