use pyo3::prelude::*;
use pyo3::types::PyModule;

mod app;
mod exceptions;
mod middlewares;
mod openapi;
mod py_handlers;
mod pydantic;
mod request;
mod responses;
mod server;
mod status;
mod utils;

pub use app::FastrAPI;
// pub use exceptions::{PyHTTPException, PyWebSocketException};
pub use request::{PyHTTPConnection, PyRequest};
pub use responses::{PyHTMLResponse, PyJSONResponse, PyPlainTextResponse, PyRedirectResponse};

use once_cell::sync::Lazy;
use papaya::HashMap;
use std::sync::Arc;

use crate::middlewares::PyMiddleware;

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
    pub param_validators: Vec<(String, Py<PyAny>)>,
    pub response_type: ResponseType,
}

pub static ROUTES: Lazy<HashMap<String, RouteHandler>> = Lazy::new(|| HashMap::with_capacity(128));

pub static MIDDLEWARES: Lazy<HashMap<String, Arc<PyMiddleware>>> =
    Lazy::new(|| HashMap::with_capacity(16));

#[pymodule(gil_used = false)]
fn fastrapi(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<FastrAPI>()?;

    responses::register(m)?;
    // exceptions::register(m)?;
    request::register(m)?;
    pydantic::register_pydantic_integration(m)?;
    status::create_status_submodule(m)?;

    middlewares::register(m)?;

    Ok(())
}
