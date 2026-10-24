//! The `SecurityScopes` class.
//! A dependency uses this class to get the OAuth2 scopes of all the dependencies in the same chain.

use pyo3::prelude::*;
use std::sync::Arc;

/// This is a special class that you can define in a parameter in a dependency to
/// obtain the OAuth2 scopes required by all the dependencies in the same chain.
///
/// This way, multiple dependencies can have different scopes, even when used in the
/// same *path operation*. And with this, you can access all the scopes required in
/// all those dependencies in a single place.
///
/// Read more about it in the
/// [FastAPI docs for OAuth2 scopes](https://fastapi.tiangolo.com/advanced/security/oauth2-scopes/).
#[pyclass(
    name = "SecurityScopes",
    module = "fastrapi.security",
    skip_from_py_object
)]
#[derive(Clone, Debug)]
pub struct PySecurityScopes {
    pub scopes: Arc<[String]>,
}

#[pymethods]
impl PySecurityScopes {
    #[new]
    #[pyo3(signature = (scopes=None))]
    #[must_use]
    pub fn new(scopes: Option<Vec<String>>) -> Self {
        Self {
            scopes: scopes.unwrap_or_default().into(),
        }
    }

    #[getter]
    fn scopes(&self) -> Vec<String> {
        self.scopes.to_vec()
    }

    #[getter]
    fn scope_str(&self) -> String {
        self.scopes.join(" ")
    }
}
