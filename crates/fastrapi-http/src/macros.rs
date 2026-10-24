//! This module contains the declarative macros of the crate.
//! The macros make Python validation exception subclasses and Python response classes.

/// Defines a subclass of the Python `ValidationException` class.
/// Use this macro to make a specific validation exception for each type of validation error.
macro_rules! define_validation_subclass {
    ($(#[$meta:meta])* $struct_name:ident, $py_name:literal) => {
        $(#[$meta])*
        #[pyo3::prelude::pyclass(extends = $crate::exceptions::PyValidationException, name = $py_name, get_all)]
        pub struct $struct_name {
            pub body: pyo3::prelude::Py<pyo3::prelude::PyAny>,
        }

        #[pyo3::prelude::pymethods]
        impl $struct_name {
            #[new]
            #[pyo3(signature = (errors, *, body=None))]
            fn new(
                py: pyo3::prelude::Python<'_>,
                errors: pyo3::prelude::Bound<'_, pyo3::prelude::PyAny>,
                body: Option<pyo3::prelude::Bound<'_, pyo3::prelude::PyAny>>,
            ) -> pyo3::prelude::PyClassInitializer<Self> {
                let body_py = body.map(|b| b.into()).unwrap_or_else(|| py.None());
                pyo3::prelude::PyClassInitializer::from($crate::exceptions::PyValidationException::new(errors))
                    .add_subclass(Self { body: body_py })
            }

            #[pyo3(signature = (*_args, **_kwargs))]
            const fn __init__(&self, _args: &pyo3::prelude::Bound<'_, pyo3::types::PyTuple>, _kwargs: Option<&pyo3::prelude::Bound<'_, pyo3::types::PyDict>>) {}
        }
    };
}

/// Defines a standard Python response class.
/// The class keeps the content, the status code, the headers, and the media type.
macro_rules! define_response_class {
    ($(#[$meta:meta])* $struct_name:ident, $py_name:literal, $content_type:ty, $default_status:expr) => {
        $(#[$meta])*
        #[pyo3::prelude::pyclass(name = $py_name, module = "fastrapi.responses", subclass, from_py_object)]
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

        #[pyo3::prelude::pymethods]
        impl $struct_name {
            #[new]
            #[pyo3(signature = (content=None, status_code=$default_status, headers=None, media_type=None, background=None))]
            fn new(
                py: pyo3::Python<'_>,
                content: Option<pyo3::Py<pyo3::PyAny>>,
                status_code: u16,
                headers: Option<pyo3::prelude::Py<pyo3::types::PyDict>>,
                media_type: Option<String>,
                background: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
            ) -> pyo3::PyResult<Self> {
                let content = content.unwrap_or_else(|| py.None());
                let media_type = media_type.or_else(|| match $py_name {
                    "HTMLResponse" => Some("text/html".to_owned()),
                    "PlainTextResponse" => Some("text/plain".to_owned()),
                    "JSONResponse" | "ORJSONResponse" | "UJSONResponse" => Some("application/json".to_owned()),
                    _ => None,
                });
                let streaming = $py_name == "StreamingResponse";
                let body = if streaming { py.None() } else { $crate::response_api::render(py, content.bind(py), $py_name, "utf-8")?.unbind() };
                let raw_headers = $crate::response_api::init_headers(py, headers.as_ref().map(|h| h.bind(py).as_any()), (!streaming).then(|| body.bind(py)), status_code, media_type.as_deref(), "utf-8")?;
                Ok(Self {
                    content,
                    status_code,
                    headers,
                    media_type,
                    background,
                    raw_headers,
                    body,
                    charset: "utf-8".to_owned(),
                })
            }
        }
        $crate::macros::response_api_methods!($struct_name, $py_name);
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
            #[pyo3(signature = (key, path=Some("/".to_owned()), domain=None, secure=false, httponly=false, samesite=Some("lax".to_owned())))]
            fn delete_cookie(&self, py: pyo3::Python<'_>, key: String, path: Option<String>, domain: Option<String>, secure: bool, httponly: bool, samesite: Option<String>) -> pyo3::PyResult<()> {
                let expires = pyo3::types::PyInt::new(py, 0);
                $crate::response_api::set_cookie(py, &self.raw_headers, key, String::new(), Some(0), Some(expires.as_any()), path, domain, secure, httponly, samesite, false)
            }
        }
    };
}

pub(crate) use define_response_class;
pub(crate) use define_validation_subclass;
pub(crate) use response_api_methods;
