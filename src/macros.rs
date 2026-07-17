#[macro_export]
macro_rules! generate_http_methods {
    ($struct_name:ident, $get_router:ident) => {
        #[pyo3::prelude::pymethods]
        impl $struct_name {
            #[pyo3(signature = (path, **kwargs), text_signature = "(self, path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=True, response_model_exclude_unset=False, response_model_exclude_defaults=False, response_model_exclude_none=False, include_in_schema=True, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=False, rate_limit=None)")]
            fn get(&self, py: pyo3::prelude::Python<'_>, path: String, kwargs: Option<&pyo3::Bound<'_, pyo3::types::PyDict>>) -> pyo3::prelude::PyResult<pyo3::prelude::Py<pyo3::prelude::PyAny>> {
                self.$get_router(py).create_method_decorator_kw(py, $crate::routing::types::HttpMethod::GET, path, kwargs)
            }
            #[pyo3(signature = (path, **kwargs), text_signature = "(self, path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=True, response_model_exclude_unset=False, response_model_exclude_defaults=False, response_model_exclude_none=False, include_in_schema=True, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=False, rate_limit=None)")]
            fn post(&self, py: pyo3::prelude::Python<'_>, path: String, kwargs: Option<&pyo3::Bound<'_, pyo3::types::PyDict>>) -> pyo3::prelude::PyResult<pyo3::prelude::Py<pyo3::prelude::PyAny>> {
                self.$get_router(py).create_method_decorator_kw(py, $crate::routing::types::HttpMethod::POST, path, kwargs)
            }
            #[pyo3(signature = (path, **kwargs), text_signature = "(self, path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=True, response_model_exclude_unset=False, response_model_exclude_defaults=False, response_model_exclude_none=False, include_in_schema=True, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=False, rate_limit=None)")]
            fn put(&self, py: pyo3::prelude::Python<'_>, path: String, kwargs: Option<&pyo3::Bound<'_, pyo3::types::PyDict>>) -> pyo3::prelude::PyResult<pyo3::prelude::Py<pyo3::prelude::PyAny>> {
                self.$get_router(py).create_method_decorator_kw(py, $crate::routing::types::HttpMethod::PUT, path, kwargs)
            }
            #[pyo3(signature = (path, **kwargs), text_signature = "(self, path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=True, response_model_exclude_unset=False, response_model_exclude_defaults=False, response_model_exclude_none=False, include_in_schema=True, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=False, rate_limit=None)")]
            fn delete(&self, py: pyo3::prelude::Python<'_>, path: String, kwargs: Option<&pyo3::Bound<'_, pyo3::types::PyDict>>) -> pyo3::prelude::PyResult<pyo3::prelude::Py<pyo3::prelude::PyAny>> {
                self.$get_router(py).create_method_decorator_kw(py, $crate::routing::types::HttpMethod::DELETE, path, kwargs)
            }
            #[pyo3(signature = (path, **kwargs), text_signature = "(self, path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=True, response_model_exclude_unset=False, response_model_exclude_defaults=False, response_model_exclude_none=False, include_in_schema=True, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=False, rate_limit=None)")]
            fn patch(&self, py: pyo3::prelude::Python<'_>, path: String, kwargs: Option<&pyo3::Bound<'_, pyo3::types::PyDict>>) -> pyo3::prelude::PyResult<pyo3::prelude::Py<pyo3::prelude::PyAny>> {
                self.$get_router(py).create_method_decorator_kw(py, $crate::routing::types::HttpMethod::PATCH, path, kwargs)
            }
            #[pyo3(signature = (path, **kwargs), text_signature = "(self, path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=True, response_model_exclude_unset=False, response_model_exclude_defaults=False, response_model_exclude_none=False, include_in_schema=True, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=False, rate_limit=None)")]
            fn options(&self, py: pyo3::prelude::Python<'_>, path: String, kwargs: Option<&pyo3::Bound<'_, pyo3::types::PyDict>>) -> pyo3::prelude::PyResult<pyo3::prelude::Py<pyo3::prelude::PyAny>> {
                self.$get_router(py).create_method_decorator_kw(py, $crate::routing::types::HttpMethod::OPTIONS, path, kwargs)
            }
            #[pyo3(signature = (path, **kwargs), text_signature = "(self, path, *, response_model=None, status_code=None, tags=None, dependencies=None, summary=None, description=None, response_description=None, responses=None, deprecated=None, operation_id=None, response_model_include=None, response_model_exclude=None, response_model_by_alias=True, response_model_exclude_unset=False, response_model_exclude_defaults=False, response_model_exclude_none=False, include_in_schema=True, response_class=None, name=None, callbacks=None, openapi_extra=None, generate_unique_id_function=None, cache_resp=False, rate_limit=None)")]
            fn head(&self, py: pyo3::prelude::Python<'_>, path: String, kwargs: Option<&pyo3::Bound<'_, pyo3::types::PyDict>>) -> pyo3::prelude::PyResult<pyo3::prelude::Py<pyo3::prelude::PyAny>> {
                self.$get_router(py).create_method_decorator_kw(py, $crate::routing::types::HttpMethod::HEAD, path, kwargs)
            }
        }
    }
}
