//! This module is the middleware layer of fastrapi.
//! It contains the built-in middleware classes, the middleware registry, and the support for Python middleware.
//! Python middleware can be a function or an ASGI middleware class.

use axum::http::header::VARY;
use axum::http::{HeaderMap, HeaderValue};

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
pub use registry::{
    DeclaredLayer, MIDDLEWARE_REGISTRY, MiddlewareBuilder, MiddlewareContainer, MiddlewareRegistry,
};
pub use session::{SessionMiddleware, parse_session_params};
pub use trustedhost::{TrustedHostMiddleware, parse_trusted_host_params};
mod python;

pub use python::{
    PreparedMiddlewares, PyLazyHeaders, PyMiddleware, PyMiddlewareKind, execute_py_middlewares,
};

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
