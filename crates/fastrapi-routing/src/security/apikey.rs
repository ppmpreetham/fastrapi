//! The API key security classes `APIKeyHeader`, `APIKeyQuery`, and `APIKeyCookie`.
//! Each class reads an API key from a header, a query parameter, or a cookie.

crate::define_api_key_security!(
    /// API key authentication using a header.
    ///
    /// This defines the name of the header that should be provided in the request with
    /// the API key and integrates that into the OpenAPI documentation. It extracts
    /// the key value sent in the header automatically and provides it as the dependency
    /// result. But it doesn't define how to send that key to the client.
    ///
    /// ## Usage
    ///
    /// Create an instance object and use that object as the dependency in `Depends()`.
    ///
    /// The dependency result will be a string containing the key value.
    ///
    /// ## Example
    ///
    /// ```python
    /// from fastrapi import Depends, FastAPI
    /// from fastrapi.security import APIKeyHeader
    ///
    /// app = FastAPI()
    ///
    /// header_scheme = APIKeyHeader(name="x-key")
    ///
    ///
    /// @app.get("/items/")
    /// async def read_items(key: str = Depends(header_scheme)):
    ///     return {"key": key}
    /// ```
    APIKeyHeader, "APIKeyHeader", header);
crate::define_api_key_security!(
    /// API key authentication using a query parameter.
    ///
    /// This defines the name of the query parameter that should be provided in the request
    /// with the API key and integrates that into the OpenAPI documentation. It extracts
    /// the key value sent in the query parameter automatically and provides it as the
    /// dependency result. But it doesn't define how to send that API key to the client.
    ///
    /// ## Usage
    ///
    /// Create an instance object and use that object as the dependency in `Depends()`.
    ///
    /// The dependency result will be a string containing the key value.
    ///
    /// ## Example
    ///
    /// ```python
    /// from fastrapi import Depends, FastAPI
    /// from fastrapi.security import APIKeyQuery
    ///
    /// app = FastAPI()
    ///
    /// query_scheme = APIKeyQuery(name="api_key")
    ///
    ///
    /// @app.get("/items/")
    /// async def read_items(api_key: str = Depends(query_scheme)):
    ///     return {"api_key": api_key}
    /// ```
    APIKeyQuery, "APIKeyQuery", query);
crate::define_api_key_security!(
    /// API key authentication using a cookie.
    ///
    /// This defines the name of the cookie that should be provided in the request with
    /// the API key and integrates that into the OpenAPI documentation. It extracts
    /// the key value sent in the cookie automatically and provides it as the dependency
    /// result. But it doesn't define how to set that cookie.
    ///
    /// ## Usage
    ///
    /// Create an instance object and use that object as the dependency in `Depends()`.
    ///
    /// The dependency result will be a string containing the key value.
    ///
    /// ## Example
    ///
    /// ```python
    /// from fastrapi import Depends, FastAPI
    /// from fastrapi.security import APIKeyCookie
    ///
    /// app = FastAPI()
    ///
    /// cookie_scheme = APIKeyCookie(name="session")
    ///
    ///
    /// @app.get("/items/")
    /// async def read_items(session: str = Depends(cookie_scheme)):
    ///     return {"session": session}
    /// ```
    APIKeyCookie, "APIKeyCookie", cookie);

#[macro_export]
macro_rules! define_api_key_security {
    ($(#[$meta:meta])* $struct_name:ident, $py_name:literal, $source:ident) => {
        $(#[$meta])*
        #[pyo3::prelude::pyclass(
                                                                    frozen,
                                                                    name = $py_name,
                                                                    module = "fastrapi.security",
                                                                    get_all,
                                                                    from_py_object,
                                                                    eq
                                                                )]
        #[derive(smart_default::SmartDefault, Clone, Debug, PartialEq, Eq)]
        pub struct $struct_name {
            pub name: String,
            pub scheme_name: Option<String>,
            pub description: Option<String>,
            #[default(true)]
            pub auto_error: bool,
        }

        #[pyo3::prelude::pymethods]
        impl $struct_name {
            #[new]
            #[pyo3(signature = (*, name, scheme_name=None, description=None, auto_error=true))]
            const fn new(
                name: String,
                scheme_name: Option<String>,
                description: Option<String>,
                auto_error: bool,
            ) -> Self {
                Self {
                    name,
                    scheme_name,
                    description,
                    auto_error,
                }
            }

            fn __call__(
                &self,
                request: &pyo3::Bound<'_, pyo3::types::PyAny>,
            ) -> pyo3::PyResult<String> {
                $crate::security::callable::api_key_call(
                    stringify!($source),
                    &self.name,
                    self.auto_error,
                    request,
                )
            }
        }
    };
}
