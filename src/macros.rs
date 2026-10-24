//! Rust macros that the `fastrapi` crate uses to generate repeated Python methods.

/// Generates the standard HTTP method decorators (`get`, `post`, `put` and the others) for a Python router class.
/// This macro removes repeated code from the definitions of the HTTP method API.
#[macro_export]
macro_rules! generate_http_methods {
    (
        $struct_name:ident,
        $get_router:ident,
        get: { $(#[$get_doc:meta])* },
        post: { $(#[$post_doc:meta])* },
        put: { $(#[$put_doc:meta])* },
        delete: { $(#[$delete_doc:meta])* },
        patch: { $(#[$patch_doc:meta])* },
        options: { $(#[$options_doc:meta])* },
        head: { $(#[$head_doc:meta])* },
    ) => {
        #[pyo3::prelude::pymethods]
        impl $struct_name {
            $(#[$get_doc])*
            #[pyo3(signature = (path, **kwargs), text_signature = "(self, path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=True, response_model_exclude_unset=False, response_model_exclude_defaults=False, response_model_exclude_none=False, include_in_schema=True, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=False, rate_limit=None)")]
            fn get(&self, py: pyo3::prelude::Python<'_>, path: String, kwargs: Option<&pyo3::Bound<'_, pyo3::types::PyDict>>) -> pyo3::prelude::PyResult<pyo3::prelude::Py<pyo3::prelude::PyAny>> {
                self.$get_router(py).create_method_decorator_kw(py, ::fastrapi_core::method::HttpMethod::GET, path, kwargs)
            }
            $(#[$post_doc])*
            #[pyo3(signature = (path, **kwargs), text_signature = "(self, path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=True, response_model_exclude_unset=False, response_model_exclude_defaults=False, response_model_exclude_none=False, include_in_schema=True, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=False, rate_limit=None)")]
            fn post(&self, py: pyo3::prelude::Python<'_>, path: String, kwargs: Option<&pyo3::Bound<'_, pyo3::types::PyDict>>) -> pyo3::prelude::PyResult<pyo3::prelude::Py<pyo3::prelude::PyAny>> {
                self.$get_router(py).create_method_decorator_kw(py, ::fastrapi_core::method::HttpMethod::POST, path, kwargs)
            }
            $(#[$put_doc])*
            #[pyo3(signature = (path, **kwargs), text_signature = "(self, path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=True, response_model_exclude_unset=False, response_model_exclude_defaults=False, response_model_exclude_none=False, include_in_schema=True, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=False, rate_limit=None)")]
            fn put(&self, py: pyo3::prelude::Python<'_>, path: String, kwargs: Option<&pyo3::Bound<'_, pyo3::types::PyDict>>) -> pyo3::prelude::PyResult<pyo3::prelude::Py<pyo3::prelude::PyAny>> {
                self.$get_router(py).create_method_decorator_kw(py, ::fastrapi_core::method::HttpMethod::PUT, path, kwargs)
            }
            $(#[$delete_doc])*
            #[pyo3(signature = (path, **kwargs), text_signature = "(self, path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=True, response_model_exclude_unset=False, response_model_exclude_defaults=False, response_model_exclude_none=False, include_in_schema=True, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=False, rate_limit=None)")]
            fn delete(&self, py: pyo3::prelude::Python<'_>, path: String, kwargs: Option<&pyo3::Bound<'_, pyo3::types::PyDict>>) -> pyo3::prelude::PyResult<pyo3::prelude::Py<pyo3::prelude::PyAny>> {
                self.$get_router(py).create_method_decorator_kw(py, ::fastrapi_core::method::HttpMethod::DELETE, path, kwargs)
            }
            $(#[$patch_doc])*
            #[pyo3(signature = (path, **kwargs), text_signature = "(self, path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=True, response_model_exclude_unset=False, response_model_exclude_defaults=False, response_model_exclude_none=False, include_in_schema=True, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=False, rate_limit=None)")]
            fn patch(&self, py: pyo3::prelude::Python<'_>, path: String, kwargs: Option<&pyo3::Bound<'_, pyo3::types::PyDict>>) -> pyo3::prelude::PyResult<pyo3::prelude::Py<pyo3::prelude::PyAny>> {
                self.$get_router(py).create_method_decorator_kw(py, ::fastrapi_core::method::HttpMethod::PATCH, path, kwargs)
            }
            $(#[$options_doc])*
            #[pyo3(signature = (path, **kwargs), text_signature = "(self, path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=True, response_model_exclude_unset=False, response_model_exclude_defaults=False, response_model_exclude_none=False, include_in_schema=True, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=False, rate_limit=None)")]
            fn options(&self, py: pyo3::prelude::Python<'_>, path: String, kwargs: Option<&pyo3::Bound<'_, pyo3::types::PyDict>>) -> pyo3::prelude::PyResult<pyo3::prelude::Py<pyo3::prelude::PyAny>> {
                self.$get_router(py).create_method_decorator_kw(py, ::fastrapi_core::method::HttpMethod::OPTIONS, path, kwargs)
            }
            $(#[$head_doc])*
            #[pyo3(signature = (path, **kwargs), text_signature = "(self, path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=True, response_model_exclude_unset=False, response_model_exclude_defaults=False, response_model_exclude_none=False, include_in_schema=True, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=False, rate_limit=None)")]
            fn head(&self, py: pyo3::prelude::Python<'_>, path: String, kwargs: Option<&pyo3::Bound<'_, pyo3::types::PyDict>>) -> pyo3::prelude::PyResult<pyo3::prelude::Py<pyo3::prelude::PyAny>> {
                self.$get_router(py).create_method_decorator_kw(py, ::fastrapi_core::method::HttpMethod::HEAD, path, kwargs)
            }
        }
    }
}
