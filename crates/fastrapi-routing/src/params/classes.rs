//! The Python parameter classes `Query`, `Path`, `Cookie`, `Header`, `Body`, `Form`, and `File`.
//! The `define_param!` macro makes each class. The file also has `define_param!` invocations for `PyDepends` and `PySecurity`.
//! An endpoint uses these classes to declare the source of each parameter value.

crate::macros::define_param!(PyQuery, "Query");
crate::macros::define_param!(
    /// Declare a path parameter for a *path operation*.
    ///
    /// Read more about it in the
    /// [FastAPI docs for Path Parameters and Numeric Validations](https://fastapi.tiangolo.com/tutorial/path-params-numeric-validations/).
    ///
    /// ```python
    /// from typing import Annotated
    ///
    /// from fastrapi import FastAPI, Path
    ///
    /// app = FastAPI()
    ///
    ///
    /// @app.get("/items/{item_id}")
    /// async def read_items(
    ///     item_id: Annotated[int, Path(title="The ID of the item to get")],
    /// ):
    ///     return {"item_id": item_id}
    /// ```
    PyPath, "Path");
crate::macros::define_param!(PyCookie, "Cookie");
crate::macros::define_param!(PyHeader, "Header", header);
crate::macros::define_param!(PyBody, "Body", body);
crate::macros::define_param!(PyForm, "Form", media: "application/x-www-form-urlencoded");
crate::macros::define_param!(PyFile, "File", media: "multipart/form-data");

crate::macros::define_param!(
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
    PyDepends, "Depends");
crate::macros::define_param!(
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
    PySecurity, "Security");
