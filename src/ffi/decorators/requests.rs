//! The HTTP method decorators of `APIRouter`: `get`, `post`, `put`, `delete`, `patch`, `options` and `head`.
//! The `generate_http_methods!` macro generates these methods.

// here lies all the decoraters signatures for get, put, patch, post, delete, options, head
// once upon a time, here lied all the methods for the above, but they were later abstracted away by heros called macros

use super::router::PyAPIRouter;
use pyo3::prelude::Python;

impl PyAPIRouter {
    #[inline]
    const fn router_ref<'a>(&'a self, _py: Python<'_>) -> &'a Self {
        self
    }
}
crate::generate_http_methods!(
    PyAPIRouter,
    router_ref,
    get: {
        /// Add a *path operation* using an HTTP GET operation.
        ///
        /// ## Example
        ///
        /// ```python
        /// from fastrapi import APIRouter, FastAPI
        ///
        /// app = FastAPI()
        /// router = APIRouter()
        ///
        /// @router.get("/items/")
        /// def read_items():
        ///     return [{"name": "Empanada"}, {"name": "Arepa"}]
        ///
        /// app.include_router(router)
        /// ```
    },
    post: {
        /// Add a *path operation* using an HTTP POST operation.
        ///
        /// ## Example
        ///
        /// ```python
        /// from fastrapi import APIRouter, FastAPI
        /// from pydantic import BaseModel
        ///
        /// class Item(BaseModel):
        ///     name: str
        ///     description: str | None = None
        ///
        /// app = FastAPI()
        /// router = APIRouter()
        ///
        /// @router.post("/items/")
        /// def create_item(item: Item):
        ///     return {"message": "Item created"}
        ///
        /// app.include_router(router)
        /// ```
    },
    put: {
        /// Add a *path operation* using an HTTP PUT operation.
        ///
        /// ## Example
        ///
        /// ```python
        /// from fastrapi import APIRouter, FastAPI
        /// from pydantic import BaseModel
        ///
        /// class Item(BaseModel):
        ///     name: str
        ///     description: str | None = None
        ///
        /// app = FastAPI()
        /// router = APIRouter()
        ///
        /// @router.put("/items/{item_id}")
        /// def replace_item(item_id: str, item: Item):
        ///     return {"message": "Item replaced", "id": item_id}
        ///
        /// app.include_router(router)
        /// ```
    },
    delete: {
        /// Add a *path operation* using an HTTP DELETE operation.
        ///
        /// ## Example
        ///
        /// ```python
        /// from fastrapi import APIRouter, FastAPI
        ///
        /// app = FastAPI()
        /// router = APIRouter()
        ///
        /// @router.delete("/items/{item_id}")
        /// def delete_item(item_id: str):
        ///     return {"message": "Item deleted"}
        ///
        /// app.include_router(router)
        /// ```
    },
    patch: {
        /// Add a *path operation* using an HTTP PATCH operation.
        ///
        /// ## Example
        ///
        /// ```python
        /// from fastrapi import APIRouter, FastAPI
        /// from pydantic import BaseModel
        ///
        /// class Item(BaseModel):
        ///     name: str
        ///     description: str | None = None
        ///
        /// app = FastAPI()
        /// router = APIRouter()
        ///
        /// @router.patch("/items/")
        /// def update_item(item: Item):
        ///     return {"message": "Item updated in place"}
        ///
        /// app.include_router(router)
        /// ```
    },
    options: {
        /// Add a *path operation* using an HTTP OPTIONS operation.
        ///
        /// ## Example
        ///
        /// ```python
        /// from fastrapi import APIRouter, FastAPI
        ///
        /// app = FastAPI()
        /// router = APIRouter()
        ///
        /// @router.options("/items/")
        /// def get_item_options():
        ///     return {"additions": ["Aji", "Guacamole"]}
        ///
        /// app.include_router(router)
        /// ```
    },
    head: {
        /// Add a *path operation* using an HTTP HEAD operation.
        ///
        /// ## Example
        ///
        /// ```python
        /// from fastrapi import APIRouter, FastAPI
        /// from pydantic import BaseModel
        ///
        /// class Item(BaseModel):
        ///     name: str
        ///     description: str | None = None
        ///
        /// app = FastAPI()
        /// router = APIRouter()
        ///
        /// @router.head("/items/", status_code=204)
        /// def get_items_headers(response: Response):
        ///     response.headers["X-Cat-Dog"] = "Alone in the world"
        ///
        /// app.include_router(router)
        /// ```
    },
);
