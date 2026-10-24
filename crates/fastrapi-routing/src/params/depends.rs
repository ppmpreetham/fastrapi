//! The Python classes `Depends` and `Security`.
//! An endpoint uses these markers to declare a dependency callable.
//! A `Security` marker can also declare OAuth2 scopes.

use pyo3::PyClassInitializer;
use pyo3::prelude::*;

/// Declare a FastAPI dependency.
///
/// It takes a single "dependable" callable (like a function).
///
/// Don't call it directly, FastAPI will call it for you.
///
/// Read more about it in the
/// [FastAPI docs for Dependencies](https://fastapi.tiangolo.com/tutorial/dependencies/).
///
/// **Example**
///
/// ```python
/// from typing import Annotated
///
/// from fastrapi import Depends, FastAPI
///
/// app = FastAPI()
///
///
/// async def common_parameters(q: str | None = None, skip: int = 0, limit: int = 100):
///     return {"q": q, "skip": skip, "limit": limit}
///
///
/// @app.get("/items/")
/// async def read_items(commons: Annotated[dict, Depends(common_parameters)]):
///     return commons
/// ```
#[pyclass(name = "Depends", subclass, get_all, from_py_object)]
#[derive(Clone)]
pub struct PyDepends {
    pub dependency: Option<Py<PyAny>>,
    pub use_cache: bool,
}

#[pymethods]
impl PyDepends {
    #[new]
    #[pyo3(signature = (dependency=None, *, use_cache=true))]
    #[must_use]
    pub const fn new(dependency: Option<Py<PyAny>>, use_cache: bool) -> Self {
        Self {
            dependency,
            use_cache,
        }
    }
}

/// Declare a FastAPI Security dependency.
///
/// The only difference with a regular dependency is that it can declare OAuth2
/// scopes that will be integrated with OpenAPI and the automatic UI docs (by default
/// at `/docs`).
///
/// It takes a single "dependable" callable (like a function).
///
/// Don't call it directly, FastAPI will call it for you.
///
/// Read more about it in the
/// [FastAPI docs for Security](https://fastapi.tiangolo.com/tutorial/security/) and
/// in the
/// [FastAPI docs for OAuth2 scopes](https://fastapi.tiangolo.com/advanced/security/oauth2-scopes/).
///
/// **Example**
///
/// ```python
/// from typing import Annotated
///
/// from fastrapi import Security, FastAPI
///
/// from .db import User
/// from .security import get_current_active_user
///
/// app = FastAPI()
///
/// @app.get("/users/me/items/")
/// async def read_own_items(
///     current_user: Annotated[User, Security(get_current_active_user, scopes=["items"])]
/// ):
///     return [{"item_id": "Foo", "owner": current_user.username}]
/// ```
#[pyclass(name = "Security", extends = PyDepends, get_all, from_py_object)]
#[derive(Clone)]
pub struct PySecurity {
    pub scopes: Vec<String>,
}

#[pymethods]
impl PySecurity {
    #[new]
    #[pyo3(signature = (dependency=None, *, scopes=None, use_cache=true))]
    fn new(
        dependency: Option<Py<PyAny>>,
        scopes: Option<Vec<String>>,
        use_cache: bool,
    ) -> pyo3::PyClassInitializer<Self> {
        PyClassInitializer::from(PyDepends::new(dependency, use_cache)).add_subclass(Self {
            scopes: scopes.unwrap_or_default(),
        })
    }
}
