use pyo3::prelude::*;

mod app;
mod cors;
mod exceptions;
mod middlewares;
mod openapi;
mod py_handlers;
mod pydantic;
mod request;
mod responses;
mod router;
mod server;
mod status;
mod utils;

pub use app::FastrAPI;
pub use exceptions::{PyHTTPException, PyWebSocketException};
pub use middlewares::{header_middleware, logging_middleware, PyMiddleware};
pub use request::{PyHTTPConnection, PyRequest};
pub use responses::{PyHTMLResponse, PyJSONResponse, PyPlainTextResponse, PyRedirectResponse};

use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::sync::Arc;

pub static ROUTES: Lazy<DashMap<String, RouteHandler>> = Lazy::new(DashMap::new);
pub static MIDDLEWARES: Lazy<DashMap<String, Arc<PyMiddleware>>> = Lazy::new(DashMap::new);

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

#[pymodule(gil_used = false)]
fn fastrapi(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<FastrAPI>()?;

    responses::register(m)?;
    exceptions::register(m)?;
    request::register(m)?;
    pydantic::register_pydantic_integration(m)?;
    status::create_status_submodule(m)?;

    cors::register(m)?;

    Ok(())
}
