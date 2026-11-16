//! This module contains the middleware registry.
//! It finds the built-in middleware that fastrapi runs natively, by class identity,
//! and records each layer the application adds with its own configuration.

use std::sync::Arc;

use fastrapi_pyutil::lazy::LazyPyAttr;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyTuple, PyType};

use super::PyMiddleware;
use super::cors::{CORSMiddleware, CorsPolicy, parse_cors_params};
use super::gzip::{GZipMiddleware, parse_gzip_params};
use super::httpsredirect::{HTTPSRedirectMiddleware, parse_https_redirect_params};
use super::session::{SessionMiddleware, parse_session_params};
use super::trustedhost::{TrustedHostMiddleware, parse_trusted_host_params};

/// A middleware layer, in the order the application declared it.
/// A built-in layer keeps its own configuration, so adding one twice keeps both.
#[derive(Clone)]
pub enum DeclaredLayer {
    Cors(Arc<CorsPolicy>),
    TrustedHost(TrustedHostMiddleware),
    HttpsRedirect,
    GZip(GZipMiddleware),
    Session(SessionMiddleware),
    Custom(Arc<PyMiddleware>),
    Decorator(Arc<PyMiddleware>),
}

fastrapi_pyutil::cached_py_import!(
    STARLETTE_CORS,
    "starlette.middleware.cors",
    "CORSMiddleware"
);
fastrapi_pyutil::cached_py_import!(
    STARLETTE_TRUSTED_HOST,
    "starlette.middleware.trustedhost",
    "TrustedHostMiddleware"
);
fastrapi_pyutil::cached_py_import!(
    STARLETTE_HTTPS_REDIRECT,
    "starlette.middleware.httpsredirect",
    "HTTPSRedirectMiddleware"
);
fastrapi_pyutil::cached_py_import!(
    STARLETTE_GZIP,
    "starlette.middleware.gzip",
    "GZipMiddleware"
);
fastrapi_pyutil::cached_py_import!(
    STARLETTE_SESSION,
    "starlette.middleware.sessions",
    "SessionMiddleware"
);

impl DeclaredLayer {
    /// The native layer for a built-in middleware class and its keyword arguments.
    /// The class is matched by identity: fastrapi's own class or the Starlette class it
    /// replaces. Any other class, even one with the same name, is not built in.
    fn native(class: &Bound<'_, PyAny>, kwargs: &Bound<'_, PyDict>) -> PyResult<Option<Self>> {
        let py = class.py();
        let is = |native: Bound<'_, PyType>, starlette: &LazyPyAttr| -> PyResult<bool> {
            Ok(class.is(&native) || class.is(starlette.get(py)?))
        };
        let layer = if is(py.get_type::<CORSMiddleware>(), &STARLETTE_CORS)? {
            Self::Cors(Arc::new(CorsPolicy::new(&parse_cors_params(kwargs)?)?))
        } else if is(
            py.get_type::<TrustedHostMiddleware>(),
            &STARLETTE_TRUSTED_HOST,
        )? {
            Self::TrustedHost(parse_trusted_host_params(kwargs)?)
        } else if is(
            py.get_type::<HTTPSRedirectMiddleware>(),
            &STARLETTE_HTTPS_REDIRECT,
        )? {
            parse_https_redirect_params(kwargs)?;
            Self::HttpsRedirect
        } else if is(py.get_type::<GZipMiddleware>(), &STARLETTE_GZIP)? {
            Self::GZip(parse_gzip_params(kwargs)?)
        } else if is(py.get_type::<SessionMiddleware>(), &STARLETTE_SESSION)? {
            Self::Session(parse_session_params(kwargs)?)
        } else {
            return Ok(None);
        };
        Ok(Some(layer))
    }

    /// The layer for a configured built-in middleware object, as in `middleware=[CORSMiddleware(...)]`.
    pub fn from_instance(item: &Bound<'_, PyAny>) -> PyResult<Option<Self>> {
        let layer = if let Ok(config) = item.cast::<CORSMiddleware>() {
            Self::Cors(Arc::new(CorsPolicy::new(&config.borrow())?))
        } else if let Ok(config) = item.cast::<TrustedHostMiddleware>() {
            Self::TrustedHost(config.borrow().clone())
        } else if item.is_instance_of::<HTTPSRedirectMiddleware>() {
            Self::HttpsRedirect
        } else if let Ok(config) = item.cast::<GZipMiddleware>() {
            Self::GZip(config.borrow().clone())
        } else if let Ok(config) = item.cast::<SessionMiddleware>() {
            Self::Session(config.borrow().clone())
        } else {
            return Ok(None);
        };
        Ok(Some(layer))
    }
}

#[derive(Clone, Default)]
pub struct MiddlewareContainer {
    pub order: Vec<DeclaredLayer>,
}

impl MiddlewareContainer {
    pub fn register(
        &mut self,
        class: &Bound<'_, PyAny>,
        args: &Bound<'_, PyTuple>,
        kwargs: &Bound<'_, PyDict>,
    ) -> PyResult<()> {
        let layer = if let Some(layer) = DeclaredLayer::native(class, kwargs)? {
            layer
        } else {
            let middleware = PyMiddleware::from_custom(class.py(), class, args, Some(kwargs))?;
            tracing::info!("Enabled custom {:?} middleware", middleware.kind);
            DeclaredLayer::Custom(Arc::new(middleware))
        };
        self.order.push(layer);
        Ok(())
    }
}
