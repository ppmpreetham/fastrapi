//! Security schemes that are compatible with `fastapi.security`.
//! This module contains the API key, HTTP, OAuth2, and OpenID Connect classes, the OAuth2 forms, and `SecurityScopes`.

pub mod callable;
pub mod forms;
pub mod utils;

mod apikey;
mod http;
mod oauth2;
mod scheme;
mod scopes;

pub use apikey::{APIKeyCookie, APIKeyHeader, APIKeyQuery};
pub use forms::{OAuth2PasswordRequestForm, OAuth2PasswordRequestFormStrict};
pub use http::{
    HTTPAuthorizationCredentials, HTTPBasic, HTTPBasicCredentials, HTTPBearer, HTTPDigest,
};
pub use oauth2::{OAuth2, OAuth2AuthorizationCodeBearer, OAuth2PasswordBearer, OpenIdConnect};
pub use scheme::{SchemeSpec, describe_scheme};
pub use scopes::PySecurityScopes;
pub use utils::register_utils_submodule;

use pyo3::intern;
use pyo3::prelude::*;

macro_rules! awaitable_call {
    ($($scheme:ty),+ $(,)?) => {$(
        #[pymethods]
        impl $scheme {
            /// Call self as a function.
            #[expect(
                clippy::unused_async_trait_impl,
                reason = "pyo3 turns this into an awaitable coroutine, like FastAPI's async __call__"
            )]
            async fn __call__(slf: Py<Self>, request: Py<PyAny>) -> PyResult<Py<PyAny>> {
                Python::attach(|py| slf.call_method1(py, intern!(py, "_resolve"), (request,)))
            }
        }
    )+};
}

awaitable_call!(
    APIKeyQuery,
    APIKeyHeader,
    APIKeyCookie,
    HTTPBearer,
    HTTPBasic,
    HTTPDigest,
    OAuth2,
    OAuth2PasswordBearer,
    OAuth2AuthorizationCodeBearer,
    OpenIdConnect,
);
