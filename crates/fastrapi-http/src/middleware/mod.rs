//! This module is the middleware layer of fastrapi.
//! It contains the built-in middleware classes, the middleware registry, and the support for Python middleware.
//! Python middleware can be a function or an ASGI middleware class.

use axum::http::header::VARY;
use axum::http::{HeaderMap, HeaderValue};
use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::PyDict;
use std::convert::Infallible;
use std::fmt;
use std::sync::Arc;

pub mod asgi;
pub mod call_next;
pub mod cors;
pub mod gzip;
pub mod httpsredirect;
pub mod registry;
mod session;
mod trustedhost;

pub use call_next::PyDownstreamResponse;
pub use cors::{CORSMiddleware, CorsPolicy, cors, parse_cors_params};
pub use gzip::{GZipMiddleware, compress, parse_gzip_params};
pub use httpsredirect::{HTTPSRedirectMiddleware, parse_https_redirect_params};
pub use registry::{DeclaredLayer, MiddlewareContainer};
pub use session::{
    PySession, SessionHandle, SessionMiddleware, SessionPolicy, parse_session_params, session,
};
pub use trustedhost::{TrustedHostMiddleware, parse_trusted_host_params};
mod python;

pub use python::{
    PreparedMiddlewares, PyLazyHeaders, PyMiddleware, PyMiddlewareKind, execute_py_middlewares,
};

#[derive(Clone)]
pub struct AsgiApp {
    app: Option<Py<PyAny>>,
    delegate: Arc<PyOnceLock<Py<PyAny>>>,
}

impl AsgiApp {
    #[must_use]
    pub fn new(app: Option<&Bound<'_, PyAny>>) -> Self {
        Self {
            app: app
                .filter(|app| !app.is_none())
                .map(|app| app.clone().unbind()),
            delegate: Arc::new(PyOnceLock::new()),
        }
    }

    pub fn call<'py>(
        &self,
        config: &Bound<'py, PyAny>,
        class: &Bound<'py, PyAny>,
        fields: &[&str],
        message: (&Bound<'py, PyAny>, &Bound<'py, PyAny>, &Bound<'py, PyAny>),
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = config.py();
        let app = self.app.as_ref().ok_or_else(|| {
            PyTypeError::new_err("the middleware was created without an ASGI app")
        })?;
        let delegate = self.delegate.get_or_try_init(py, || {
            let kwargs = PyDict::new(py);
            for field in fields {
                kwargs.set_item(field, config.getattr(field)?)?;
            }
            class.call((app,), Some(&kwargs)).map(Bound::unbind)
        })?;
        delegate.bind(py).call1(message)
    }
}

impl Default for AsgiApp {
    fn default() -> Self {
        Self::new(None)
    }
}

impl PartialEq for AsgiApp {
    fn eq(&self, other: &Self) -> bool {
        self.app.as_ref().map(Py::as_ptr) == other.app.as_ref().map(Py::as_ptr)
    }
}

impl Eq for AsgiApp {}

impl fmt::Debug for AsgiApp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AsgiApp")
            .field("app", &self.app)
            .finish_non_exhaustive()
    }
}

impl<'py> IntoPyObject<'py> for &AsgiApp {
    type Target = PyAny;
    type Output = Bound<'py, PyAny>;
    type Error = Infallible;

    fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
        Ok(self
            .app
            .as_ref()
            .map_or_else(|| py.None().into_bound(py), |app| app.bind(py).clone()))
    }
}

macro_rules! asgi_middleware {
    ($ty:ty, $module:literal, $class:literal, [$($field:literal),*]) => {
        #[pyo3::pymethods]
        impl $ty {
            fn __call__<'py>(
                slf: &pyo3::Bound<'py, Self>,
                scope: &pyo3::Bound<'py, pyo3::PyAny>,
                receive: &pyo3::Bound<'py, pyo3::PyAny>,
                send: &pyo3::Bound<'py, pyo3::PyAny>,
            ) -> pyo3::PyResult<pyo3::Bound<'py, pyo3::PyAny>> {
                fastrapi_pyutil::cached_py_import!(STARLETTE, $module, $class);
                slf.get().app.call(
                    slf.as_any(),
                    STARLETTE.get(slf.py())?,
                    &[$($field),*],
                    (scope, receive, send),
                )
            }
        }
    };
}

pub(crate) use asgi_middleware;

fn add_vary(headers: &mut HeaderMap, value: &'static str) {
    let vary = match headers.get(VARY) {
        Some(existing) => {
            HeaderValue::from_bytes(&[existing.as_bytes(), b", ", value.as_bytes()].concat())
        }
        None => Ok(HeaderValue::from_static(value)),
    };
    if let Ok(vary) = vary {
        headers.insert(VARY, vary);
    }
}
