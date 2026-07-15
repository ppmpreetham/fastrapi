use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyModule};
use pyo3_nest::{add_classes, submodule};

pub mod engine;
pub mod ffi;
mod globals;
pub mod http;
pub mod routing;
pub mod types;
pub mod utils;

pub use engine::app;
pub use engine::background;
pub use engine::server;
pub use ffi::datastructures;
pub use ffi::decorators;
pub use ffi::exceptions;
pub use ffi::py_handlers;
pub use ffi::pydantic;
pub use globals::{BASEMODEL_TYPE, MIDDLEWARES, PYTHON_RUNTIME, config};
pub use http::middleware;
pub use http::request;
pub use http::responses;
pub use http::staticfiles;
pub use http::status;
pub use http::websocket;
pub use routing::dependencies;
pub use routing::params;
pub use routing::security;

pub use app::FastrAPI;
pub use request::{PyHTTPConnection, PyRequest};
pub use responses::{
    PyHTMLResponse, PyJSONResponse, PyPlainTextResponse, PyRedirectResponse, PyStreamingResponse,
};

use crate::routing::security::{
    APIKeyCookie, APIKeyHeader, APIKeyQuery, HTTPAuthorizationCredentials, HTTPBasic,
    HTTPBasicCredentials, HTTPBearer, OAuth2PasswordBearer, PySecurityScopes,
};
use background::PyBackgroundTasks;
use datastructures::PyUploadFile;
use decorators::PyAPIRouter;
use exceptions::{
    PyFastrAPIDeprecationWarning, PyFastrAPIError, PyHTTPException, PyRequestValidationError,
    PyResponseValidationError, PyValidationException, PyWebSocketException,
};
use middleware::{CORSMiddleware, GZipMiddleware, SessionMiddleware, TrustedHostMiddleware};
use params::{
    PyBody, PyCookie, PyDepends, PyFile, PyForm, PyHeader, PyPath, PyQuery, PySecurity, Undefined,
    Unset,
};
use routing::prometheus::PyInstrumentator;
use staticfiles::PyStaticFiles;
use websocket::PyWebSocket;

fn register_rsloop_asyncio_alias(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let rsloop_module = py.import("rsloop")?;
    m.add("asyncio", &rsloop_module)?;

    let sys_modules = py
        .import("sys")?
        .getattr("modules")?
        .cast_into::<PyDict>()?;
    sys_modules.set_item("fastrapi.asyncio", rsloop_module)?;
    Ok(())
}
#[pymodule(gil_used = false)]
fn fastrapi(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.setattr("__package__", "fastrapi")?;
    m.setattr("__path__", PyList::empty(py))?;

    m.add_class::<FastrAPI>()?;

    submodule!(
        m,
        "responses",
        add_classes!(
            PyJSONResponse,
            PyHTMLResponse,
            PyPlainTextResponse,
            PyRedirectResponse,
            PyStreamingResponse
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
    submodule!(
        m,
        "security",
        add_classes!(
            PySecurityScopes,
            APIKeyHeader,
            APIKeyQuery,
            APIKeyCookie,
            HTTPAuthorizationCredentials,
            HTTPBearer,
            OAuth2PasswordBearer,
            HTTPBasicCredentials,
            HTTPBasic
        )
    );
    submodule!(m, "staticfiles", add_classes!(PyStaticFiles));
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
    submodule!(m, "prometheus", add_classes!(PyInstrumentator));
    m.add(
        "Instrumentator",
        m.getattr("prometheus")?.getattr("Instrumentator")?,
    )?;
    submodule!(m, "websocket", add_classes!(PyWebSocket));

    status::create_status_submodule(m)?;
    pydantic::register_pydantic_integration(m)?;
    register_rsloop_asyncio_alias(m)?;

    m.add(
        "SecurityScopes",
        m.getattr("security")?.getattr("SecurityScopes")?,
    )?;
    m.add(
        "OAuth2PasswordBearer",
        m.getattr("security")?.getattr("OAuth2PasswordBearer")?,
    )?;
    m.add("HTTPBearer", m.getattr("security")?.getattr("HTTPBearer")?)?;
    m.add("HTTPBasic", m.getattr("security")?.getattr("HTTPBasic")?)?;
    m.add(
        "APIKeyHeader",
        m.getattr("security")?.getattr("APIKeyHeader")?,
    )?;
    m.add(
        "APIKeyQuery",
        m.getattr("security")?.getattr("APIKeyQuery")?,
    )?;
    m.add(
        "APIKeyCookie",
        m.getattr("security")?.getattr("APIKeyCookie")?,
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
        "StaticFiles",
        m.getattr("staticfiles")?.getattr("StaticFiles")?,
    )?;
    m.add(
        "UploadFile",
        m.getattr("datastructures")?.getattr("UploadFile")?,
    )?;
    m.add_class::<PyAPIRouter>()?;

    Ok(())
}
