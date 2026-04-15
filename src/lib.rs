use pyo3::prelude::*;
use pyo3::types::PyModule;
use pyo3_nest::{add_classes, submodule};

mod app;
pub mod bridge;
pub mod config;
pub mod core;
pub mod datastructures;
pub mod middleware;
pub mod openapi;
pub mod params;
pub mod python;
pub mod security;
mod server;
pub mod types;
pub mod utils;

pub use app::FastrAPI;
pub use core::{RouteHandler, ROUTES};
pub use middleware::MIDDLEWARES;
pub use python::response::{
    PyHTMLResponse, PyJSONResponse, PyPlainTextResponse, PyRedirectResponse,
};
pub use types::request::{PyHTTPConnection, PyRequest};
pub use types::response::ResponseType;

pub(crate) use bridge::call_python as py_handlers;
pub(crate) use middleware as middlewares;
pub(crate) use python::dependencies;
pub(crate) use python::models as pydantic;
pub(crate) use python::response as responses;
pub(crate) use python::websocket as websocket;
pub(crate) use types::status as status;

use crate::datastructures::PyUploadFile;
use crate::python::background::PyBackgroundTasks;
use crate::python::exceptions::{
    PyFastrAPIDeprecationWarning, PyFastrAPIError, PyHTTPException, PyRequestValidationError,
    PyResponseValidationError, PyValidationException, PyWebSocketException,
};
use crate::python::websocket::PyWebSocket;
use crate::security::PySecurityScopes;
use middleware::{CORSMiddleware, GZipMiddleware, SessionMiddleware, TrustedHostMiddleware};
use params::{
    PyBody, PyCookie, PyDepends, PyFile, PyForm, PyHeader, PyPath, PyQuery, PySecurity,
    Undefined, Unset,
};

#[pymodule(gil_used = false)]
fn fastrapi(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.setattr("__package__", "fastrapi")?;
    m.setattr("__path__", py.eval(c"[]", None, None)?)?;

    m.add_class::<FastrAPI>()?;
    m.add("FastrAPI", m.getattr("FastrAPI")?)?;

    submodule!(
        m,
        "responses",
        add_classes!(
            PyJSONResponse,
            PyHTMLResponse,
            PyPlainTextResponse,
            PyRedirectResponse
        )
    );

    submodule!(
        m,
        "exceptions",
        add_classes!(
            PyFastrAPIError,
            PyValidationException,
            PyRequestValidationError,
            PyResponseValidationError,
            PyHTTPException,
            PyWebSocketException,
            PyFastrAPIDeprecationWarning
        )
    );

    submodule!(
        m,
        "params",
        add_classes!(
            PyQuery, PyPath, PyBody, PyCookie, PyHeader, PyForm, PyFile, Unset, Undefined,
            PyDepends, PySecurity
        )
    );

    submodule!(m, "request", add_classes!(PyRequest, PyHTTPConnection));
    submodule!(m, "datastructures", add_classes!(PyUploadFile));
    submodule!(m, "background", add_classes!(PyBackgroundTasks));
    submodule!(m, "security", add_classes!(PySecurityScopes));
    submodule!(
        m,
        "middleware",
        add_classes!(
            CORSMiddleware,
            TrustedHostMiddleware,
            GZipMiddleware,
            SessionMiddleware
        )
    );

    submodule!(m, "middleware.cors", add_classes!(CORSMiddleware));
    submodule!(m, "websocket", add_classes!(PyWebSocket));
    let ws_mod = m.getattr("websocket")?.cast_into::<PyModule>()?;
    ws_mod.add_function(wrap_pyfunction!(crate::python::websocket::websocket, &ws_mod)?)?;

    status::create_status_submodule(m)?;
    pydantic::register_pydantic_integration(m)?;

    m.add("Depends", m.getattr("params")?.getattr("Depends")?)?;
    m.add("Query", m.getattr("params")?.getattr("Query")?)?;
    m.add("Path", m.getattr("params")?.getattr("Path")?)?;
    m.add("Body", m.getattr("params")?.getattr("Body")?)?;
    m.add(
        "HTTPException",
        m.getattr("exceptions")?.getattr("HTTPException")?,
    )?;
    m.add(
        "BackgroundTasks",
        m.getattr("background")?.getattr("BackgroundTasks")?,
    )?;
    m.add("Header", m.getattr("params")?.getattr("Header")?)?;
    m.add("Cookie", m.getattr("params")?.getattr("Cookie")?)?;
    m.add("Form", m.getattr("params")?.getattr("Form")?)?;
    m.add("File", m.getattr("params")?.getattr("File")?)?;
    m.add("Security", m.getattr("params")?.getattr("Security")?)?;
    m.add(
        "UploadFile",
        m.getattr("datastructures")?.getattr("UploadFile")?,
    )?;

    Ok(())
}
