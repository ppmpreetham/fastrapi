//! The Python classes `Depends` and `Security`.
//! An endpoint uses these markers to declare a dependency callable.
//! A `Security` marker can also declare OAuth2 scopes.

use pyo3::PyClassInitializer;
use pyo3::basic::CompareOp;
use pyo3::prelude::*;
use pyo3::types::PyTuple;

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
    pub scope: Option<Py<PyAny>>,
}

impl PyDepends {
    fn fields<'py>(&self, py: Python<'py>) -> [Bound<'py, PyAny>; 3] {
        [
            optional(py, self.dependency.as_ref()),
            pyo3::types::PyBool::new(py, self.use_cache).to_owned().into_any(),
            optional(py, self.scope.as_ref()),
        ]
    }
}

fn optional<'py>(py: Python<'py>, value: Option<&Py<PyAny>>) -> Bound<'py, PyAny> {
    value.map_or_else(|| py.None().into_bound(py), |value| value.bind(py).clone())
}

fn fields_repr(name: &str, names: &[&str], values: &[Bound<'_, PyAny>]) -> PyResult<String> {
    let mut out = format!("{name}(");
    for (index, (field, value)) in names.iter().zip(values).enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(field);
        out.push('=');
        out.push_str(value.repr()?.to_str()?);
    }
    out.push(')');
    Ok(out)
}

fn fields_eq<'py>(
    slf: &Bound<'py, PyAny>,
    other: &Bound<'py, PyAny>,
    op: CompareOp,
    fields: impl Fn(&Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>>,
) -> PyResult<Py<PyAny>> {
    let py = slf.py();
    if !matches!(op, CompareOp::Eq | CompareOp::Ne) || !other.get_type().is(slf.get_type()) {
        return Ok(py.NotImplemented());
    }
    let equal = fields(slf)?.eq(fields(other)?)?;
    Ok(pyo3::types::PyBool::new(py, equal == matches!(op, CompareOp::Eq))
        .to_owned()
        .into_any()
        .unbind())
}

fn depends_tuple<'py>(value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    let depends = value.cast::<PyDepends>()?.borrow();
    PyTuple::new(value.py(), depends.fields(value.py()))
}

fn security_fields<'py>(value: &Bound<'py, PyAny>) -> PyResult<[Bound<'py, PyAny>; 4]> {
    let py = value.py();
    let security = value.cast::<PySecurity>()?;
    let [dependency, use_cache, scope] = security.as_super().borrow().fields(py);
    let scopes = optional(py, security.borrow().scopes.as_ref());
    Ok([dependency, use_cache, scope, scopes])
}

fn security_tuple<'py>(value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    PyTuple::new(value.py(), security_fields(value)?)
}

#[pymethods]
impl PyDepends {
    #[new]
    #[pyo3(signature = (dependency=None, *, use_cache=true, scope=None))]
    #[must_use]
    pub const fn new(
        dependency: Option<Py<PyAny>>,
        use_cache: bool,
        scope: Option<Py<PyAny>>,
    ) -> Self {
        Self {
            dependency,
            use_cache,
            scope,
        }
    }

    fn __repr__(slf: &Bound<'_, Self>) -> PyResult<String> {
        let name = slf.get_type().qualname()?;
        let values = slf.borrow().fields(slf.py());
        fields_repr(name.to_str()?, &["dependency", "use_cache", "scope"], &values)
    }

    fn __richcmp__(
        slf: &Bound<'_, Self>,
        other: &Bound<'_, PyAny>,
        op: CompareOp,
    ) -> PyResult<Py<PyAny>> {
        fields_eq(slf.as_any(), other, op, depends_tuple)
    }

    fn __hash__(slf: &Bound<'_, Self>) -> PyResult<isize> {
        depends_tuple(slf.as_any())?.hash()
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
    pub scopes: Option<Py<PyAny>>,
}

#[pymethods]
impl PySecurity {
    #[new]
    #[pyo3(signature = (dependency=None, *, scopes=None, use_cache=true, scope=None))]
    fn new(
        dependency: Option<Py<PyAny>>,
        scopes: Option<Py<PyAny>>,
        use_cache: bool,
        scope: Option<Py<PyAny>>,
    ) -> pyo3::PyClassInitializer<Self> {
        PyClassInitializer::from(PyDepends::new(dependency, use_cache, scope))
            .add_subclass(Self { scopes })
    }

    fn __repr__(slf: &Bound<'_, Self>) -> PyResult<String> {
        let name = slf.get_type().qualname()?;
        let values = security_fields(slf.as_any())?;
        fields_repr(
            name.to_str()?,
            &["dependency", "use_cache", "scope", "scopes"],
            &values,
        )
    }

    fn __richcmp__(
        slf: &Bound<'_, Self>,
        other: &Bound<'_, PyAny>,
        op: CompareOp,
    ) -> PyResult<Py<PyAny>> {
        fields_eq(slf.as_any(), other, op, security_tuple)
    }

    fn __hash__(slf: &Bound<'_, Self>) -> PyResult<isize> {
        security_tuple(slf.as_any())?.hash()
    }
}
