//! This module contains the declarative macros of the crate.
//! The macros make the Python response classes.

/// Defines a standard Python response class.
/// The class keeps the content, the status code, the headers, and the media type.
macro_rules! define_response_class {
    (@struct [$($pyclass:tt)*] $(#[$meta:meta])* $struct_name:ident, $py_name:literal) => {
        $(#[$meta])*
        #[pyo3::prelude::pyclass(name = $py_name, module = "fastrapi.responses", $($pyclass)* subclass, from_py_object)]
        #[derive(Clone)]
        pub struct $struct_name {
            #[pyo3(get, set)]
            pub content: pyo3::Py<pyo3::PyAny>,
            #[pyo3(get, set)]
            pub status_code: u16,
            pub headers: Option<pyo3::prelude::Py<pyo3::types::PyDict>>,
            #[pyo3(get, set)]
            pub media_type: Option<String>,
            #[pyo3(get, set)]
            pub background: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
            #[pyo3(get, set)]
            pub raw_headers: pyo3::Py<pyo3::types::PyList>,
            #[pyo3(get, set)]
            pub body: pyo3::Py<pyo3::PyAny>,
            #[pyo3(get, set)]
            pub charset: String,
        }

        impl $struct_name {
            fn from_response_parts(parts: $crate::response_api::ResponseParts) -> Self {
                Self {
                    content: parts.content,
                    status_code: parts.status_code,
                    headers: parts.headers,
                    media_type: parts.media_type,
                    background: parts.background,
                    raw_headers: parts.raw_headers,
                    body: parts.body,
                    charset: parts.charset,
                }
            }
        }

        $crate::macros::response_api_methods!($struct_name, $py_name);

        #[pyo3::pymethods]
        impl $struct_name {
            /// The `__init__` of a subclass that overrides `render`: it renders the body
            /// and its headers with that `render`, like Starlette's `Response.__init__`.
            #[pyo3(signature = (*_args, **_kwargs))]
            fn _render_with_override(
                slf: &pyo3::Bound<'_, Self>,
                _args: &pyo3::Bound<'_, pyo3::types::PyTuple>,
                _kwargs: Option<&pyo3::Bound<'_, pyo3::types::PyDict>>,
            ) -> pyo3::PyResult<()> {
                let py = slf.py();
                let content = slf.borrow().content.clone_ref(py);
                let body = slf.call_method1(pyo3::intern!(py, "render"), (content,))?;
                let mut response = slf.borrow_mut();
                response.raw_headers = $crate::response_api::init_headers(py, response.headers.as_ref().map(|headers| headers.bind(py).as_any()), Some(&body), response.status_code, response.media_type.as_deref(), &response.charset)?;
                response.body = body.unbind();
                Ok(())
            }
        }
    };
    (@base $(#[$meta:meta])* $struct_name:ident, $py_name:literal, $default_status:expr) => {
        $crate::macros::define_response_class!(@struct [] $(#[$meta])* $struct_name, $py_name);

        #[pyo3::prelude::pymethods]
        impl $struct_name {
            #[new]
            #[classmethod]
            #[pyo3(signature = (content=None, status_code=$default_status, headers=None, media_type=None, background=None))]
            fn new(
                cls: &pyo3::Bound<'_, pyo3::types::PyType>,
                content: Option<pyo3::Py<pyo3::PyAny>>,
                status_code: u16,
                headers: Option<$crate::response_api::HeadersArg>,
                media_type: Option<String>,
                background: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
            ) -> pyo3::PyResult<Self> {
                let overrides = $crate::response_api::RenderOverrides::of::<Self>(cls);
                $crate::response_api::response_parts(cls.py(), $py_name, overrides, content, status_code, headers.map(|headers| headers.0), media_type, background)
                    .map(Self::from_response_parts)
            }

            #[classmethod]
            #[pyo3(signature = (**_kwargs))]
            fn __init_subclass__(
                cls: &pyo3::Bound<'_, pyo3::types::PyType>,
                _kwargs: Option<&pyo3::Bound<'_, pyo3::types::PyDict>>,
            ) -> pyo3::PyResult<()> {
                $crate::response_api::RenderOverrides::register(cls)
            }
        }
    };
    ($(#[$meta:meta])* $struct_name:ident, $py_name:literal, $default_status:expr) => {
        $crate::macros::define_response_class!(@struct [extends = $crate::responses::PyResponse,] $(#[$meta])* $struct_name, $py_name);

        #[pyo3::prelude::pymethods]
        impl $struct_name {
            #[new]
            #[classmethod]
            #[pyo3(signature = (content=None, status_code=$default_status, headers=None, media_type=None, background=None))]
            fn new(
                cls: &pyo3::Bound<'_, pyo3::types::PyType>,
                content: Option<pyo3::Py<pyo3::PyAny>>,
                status_code: u16,
                headers: Option<$crate::response_api::HeadersArg>,
                media_type: Option<String>,
                background: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
            ) -> pyo3::PyResult<pyo3::PyClassInitializer<Self>> {
                let py = cls.py();
                let overrides = $crate::response_api::RenderOverrides::of::<Self>(cls);
                let parts = $crate::response_api::response_parts(py, $py_name, overrides, content, status_code, headers.map(|headers| headers.0), media_type, background)?;
                Ok(pyo3::PyClassInitializer::from($crate::responses::PyResponse::empty(py)?)
                    .add_subclass(Self::from_response_parts(parts)))
            }
        }
    };
}

macro_rules! response_api_methods {
    ($struct_name:ident, $py_name:literal) => {
        #[pyo3::pymethods]
        impl $struct_name {
            #[getter]
            fn headers<'py>(&self, py: pyo3::Python<'py>) -> pyo3::PyResult<pyo3::Bound<'py, pyo3::PyAny>> {
                $crate::response_api::headers(py, &self.raw_headers)
            }
            fn render<'py>(&self, py: pyo3::Python<'py>, content: &pyo3::Bound<'py, pyo3::PyAny>) -> pyo3::PyResult<pyo3::Bound<'py, pyo3::PyAny>> {
                $crate::response_api::render(py, content, $py_name, &self.charset)
            }
            #[pyo3(signature = (headers=None))]
            fn init_headers(&mut self, py: pyo3::Python<'_>, headers: Option<&pyo3::Bound<'_, pyo3::PyAny>>) -> pyo3::PyResult<()> {
                self.raw_headers = $crate::response_api::init_headers(py, headers, (!self.body.bind(py).is_none()).then(|| self.body.bind(py)), self.status_code, self.media_type.as_deref(), &self.charset)?;
                Ok(())
            }
            #[pyo3(signature = (key, value="".to_owned(), max_age=None, expires=None, path=Some("/".to_owned()), domain=None, secure=false, httponly=false, samesite=Some("lax".to_owned()), partitioned=false))]
            fn set_cookie(&self, py: pyo3::Python<'_>, key: String, value: String, max_age: Option<i64>, expires: Option<&pyo3::Bound<'_, pyo3::PyAny>>, path: Option<String>, domain: Option<String>, secure: bool, httponly: bool, samesite: Option<String>, partitioned: bool) -> pyo3::PyResult<()> {
                $crate::response_api::set_cookie(py, &self.raw_headers, key, value, max_age, expires, path, domain, secure, httponly, samesite, partitioned)
            }
            fn __call__<'py>(
                slf: &pyo3::Bound<'py, Self>,
                _scope: &pyo3::Bound<'py, pyo3::PyAny>,
                _receive: &pyo3::Bound<'py, pyo3::PyAny>,
                send: pyo3::Bound<'py, pyo3::PyAny>,
            ) -> pyo3::PyResult<pyo3::Bound<'py, pyo3::PyAny>> {
                $crate::response_api::asgi_call(slf.as_any(), send)
            }
            #[pyo3(signature = (key, path=Some("/".to_owned()), domain=None, secure=false, httponly=false, samesite=Some("lax".to_owned()), partitioned=false))]
            fn delete_cookie(&self, py: pyo3::Python<'_>, key: String, path: Option<String>, domain: Option<String>, secure: bool, httponly: bool, samesite: Option<String>, partitioned: bool) -> pyo3::PyResult<()> {
                let expires = pyo3::types::PyInt::new(py, 0);
                $crate::response_api::set_cookie(py, &self.raw_headers, key, String::new(), Some(0), Some(expires.as_any()), path, domain, secure, httponly, samesite, partitioned)
            }
        }
    };
}

pub(crate) use define_response_class;
pub(crate) use response_api_methods;
