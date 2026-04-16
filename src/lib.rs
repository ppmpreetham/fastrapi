use pyo3::prelude::*;
use pyo3::types::PyModule;
use pyo3_nest::{add_classes, submodule};

mod app;
mod background;
mod config;
mod datastructures;
mod dependencies;
mod exceptions;
mod globals;
mod middleware;
mod openapi;
mod params;
mod py_handlers;
mod pydantic;
mod request;
mod responses;
mod security;
mod server;
mod status;
pub mod types;
mod utils;
mod websocket;
pub use app::FastrAPI;
pub use globals::{config, BASEMODEL_TYPE, MIDDLEWARES, PYTHON_RUNTIME, ROUTES, WEBSOCKET_ROUTES};
use papaya::HashMap as PapayaHashMap;
pub use request::{PyHTTPConnection, PyRequest};
pub use responses::{PyHTMLResponse, PyJSONResponse, PyPlainTextResponse, PyRedirectResponse};

use background::PyBackgroundTasks;
use datastructures::PyUploadFile;
use exceptions::{
    PyFastrAPIDeprecationWarning, PyFastrAPIError, PyHTTPException, PyRequestValidationError,
    PyResponseValidationError, PyValidationException, PyWebSocketException,
};
use middleware::{CORSMiddleware, GZipMiddleware, SessionMiddleware, TrustedHostMiddleware};
use params::{
    PyBody, PyCookie, PyDepends, PyFile, PyForm, PyHeader, PyPath, PyQuery, PySecurity, Undefined,
    Unset,
};
use security::PySecurityScopes;
use websocket::PyWebSocket;

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

    // backward compatibility: fastrapi.middleware.cors
    submodule!(m, "middleware.cors", add_classes!(CORSMiddleware));
    submodule!(m, "websocket", add_classes!(PyWebSocket));
    let ws_mod = m.getattr("websocket")?.cast_into::<PyModule>()?;
    ws_mod.add_function(wrap_pyfunction!(crate::websocket::websocket, &ws_mod)?)?;

    status::create_status_submodule(m)?;
    pydantic::register_pydantic_integration(m)?;

    // top level re-exports
    // for `from fastrapi import Query, Depends, HTTPException`
    m.add(
        "SecurityScopes",
        m.getattr("security")?.getattr("SecurityScopes")?,
    )?;
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
