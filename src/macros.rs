/// Generates standard HTTP method decorators (get, post, put, etc.) for a Python router class.
/// This reduces boilerplate when defining the API for HTTP methods.
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

/// Matches a standard HTTP method to the corresponding axum router function.
/// This connects the Python HTTP methods to the Rust axum router.
#[macro_export]
macro_rules! match_method_router {
    ($method:expr, $handler:expr) => {
        match $method {
            HttpMethod::GET => get($handler),
            HttpMethod::POST => post($handler),
            HttpMethod::PUT => put($handler),
            HttpMethod::DELETE => delete($handler),
            HttpMethod::PATCH => patch($handler),
            HttpMethod::OPTIONS => options($handler),
            HttpMethod::HEAD => head($handler),
        }
    };
}

/// Defines a subclass for a Python validation exception.
/// This makes it easy to create specific validation exceptions for different types of validation errors.
#[macro_export]
macro_rules! define_validation_subclass {
    ($struct_name:ident, $py_name:literal) => {
        #[pyo3::prelude::pyclass(extends = $crate::ffi::exceptions::PyValidationException, name = $py_name, get_all)]
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
                pyo3::prelude::PyClassInitializer::from($crate::ffi::exceptions::PyValidationException::new(errors))
                    .add_subclass(Self { body: body_py })
            }

            #[pyo3(signature = (*_args, **_kwargs))]
            fn __init__(&self, _args: &pyo3::prelude::Bound<'_, pyo3::types::PyTuple>, _kwargs: Option<&pyo3::prelude::Bound<'_, pyo3::types::PyDict>>) {}
        }
    };
}

/// Defines a standard Python response class.
/// This encapsulates content, status code, headers, and media types into a Python class format.
#[macro_export]
macro_rules! define_response_class {
    ($struct_name:ident, $py_name:literal, $content_type:ty, $default_status:expr) => {
        #[pyo3::prelude::pyclass(name = $py_name, get_all, set_all, from_py_object)]
        #[derive(Clone)]
        pub struct $struct_name {
            pub content: $content_type,
            pub status_code: u16,
            pub headers: Option<pyo3::prelude::Py<pyo3::types::PyDict>>,
            pub media_type: Option<String>,
            pub background: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
        }

        #[pyo3::prelude::pymethods]
        impl $struct_name {
            #[new]
            #[pyo3(signature = (content, status_code=$default_status, headers=None, media_type=None, background=None))]
            fn new(
                content: $content_type,
                status_code: u16,
                headers: Option<pyo3::prelude::Py<pyo3::types::PyDict>>,
                media_type: Option<String>,
                background: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
            ) -> Self {
                Self {
                    content,
                    status_code,
                    headers,
                    media_type,
                    background,
                }
            }
        }
    };
}

/// Internal macro used to construct parameter types.
/// It defines the Python class structure, properties, and methods for a parameter.
#[macro_export]
macro_rules! define_param_internal {
    (
        $struct_name:ident,
        $py_name:literal,
        extra_fields: { $( $extra_field:vis $extra_field_name:ident : $extra_field_type:ty , )* },
        extra_sig: { $( $extra_sig_name:ident = $extra_sig_val:expr , )* },
        extra_args: { $( $extra_arg_name:ident : $extra_arg_type:ty , )* },
        extra_init: { $( $extra_init_name:ident , )* }
    ) => {
        #[pyo3::prelude::pyclass(name = $py_name, get_all, set_all, from_py_object)]
        #[derive(Clone)]
        pub struct $struct_name {
            pub default: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
            pub default_factory: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
            pub alias: Option<String>,
            pub alias_priority: Option<i32>,
            pub validation_alias: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
            pub serialization_alias: Option<String>,
            pub title: Option<String>,
            pub description: Option<String>,
            pub gt: Option<f64>,
            pub ge: Option<f64>,
            pub lt: Option<f64>,
            pub le: Option<f64>,
            pub min_length: Option<usize>,
            pub max_length: Option<usize>,
            pub pattern: Option<String>,
            pub regex: Option<String>,
            pub discriminator: Option<String>,
            pub strict: Option<bool>,
            pub multiple_of: Option<f64>,
            pub allow_inf_nan: Option<bool>,
            pub max_digits: Option<usize>,
            pub decimal_places: Option<usize>,
            pub examples: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
            pub example: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
            pub openapi_examples: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
            pub deprecated: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
            pub include_in_schema: bool,
            pub json_schema_extra: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
            $( $extra_field $extra_field_name : $extra_field_type , )*
        }

        #[pyo3::prelude::pymethods]
        impl $struct_name {
            #[new]
            #[pyo3(signature = (
                default = None,
                *,
                default_factory = None,
                $( $extra_sig_name = $extra_sig_val , )*
                alias = None,
                alias_priority = None,
                validation_alias = None,
                serialization_alias = None,
                title = None,
                description = None,
                gt = None,
                ge = None,
                lt = None,
                le = None,
                min_length = None,
                max_length = None,
                pattern = None,
                regex = None,
                discriminator = None,
                strict = None,
                multiple_of = None,
                allow_inf_nan = None,
                max_digits = None,
                decimal_places = None,
                examples = None,
                example = None,
                openapi_examples = None,
                deprecated = None,
                include_in_schema = true,
                json_schema_extra = None,
                **_extra
            ))]
            #[allow(clippy::too_many_arguments)]
            fn new(
                default: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
                default_factory: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
                $( $extra_arg_name : $extra_arg_type , )*
                alias: Option<String>,
                alias_priority: Option<i32>,
                validation_alias: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
                serialization_alias: Option<String>,
                title: Option<String>,
                description: Option<String>,
                gt: Option<f64>,
                ge: Option<f64>,
                lt: Option<f64>,
                le: Option<f64>,
                min_length: Option<usize>,
                max_length: Option<usize>,
                pattern: Option<String>,
                regex: Option<String>,
                discriminator: Option<String>,
                strict: Option<bool>,
                multiple_of: Option<f64>,
                allow_inf_nan: Option<bool>,
                max_digits: Option<usize>,
                decimal_places: Option<usize>,
                examples: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
                example: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
                openapi_examples: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
                deprecated: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
                include_in_schema: bool,
                json_schema_extra: Option<pyo3::prelude::Py<pyo3::prelude::PyAny>>,
                _extra: Option<&pyo3::prelude::Bound<'_, pyo3::types::PyDict>>,
            ) -> Self {
                Self {
                    default,
                    default_factory,
                    alias,
                    alias_priority,
                    validation_alias,
                    serialization_alias,
                    title,
                    description,
                    gt,
                    ge,
                    lt,
                    le,
                    min_length,
                    max_length,
                    pattern,
                    regex,
                    discriminator,
                    strict,
                    multiple_of,
                    allow_inf_nan,
                    max_digits,
                    decimal_places,
                    examples,
                    example,
                    openapi_examples,
                    deprecated,
                    include_in_schema,
                    json_schema_extra,
                    $( $extra_init_name , )*
                }
            }
        }
    };
}

/// Defines HTTP parameter classes (Query, Path, Header, etc.) for the Python API.
/// Provides multiple signatures to support aliases, descriptions, constraints, and dependencies.
#[macro_export]
macro_rules! define_param {
    // Base parameter (Query, Path, Cookie)
    ($struct_name:ident, $py_name:literal) => {
        $crate::define_param_internal!(
            $struct_name,
            $py_name,
            extra_fields: {},
            extra_sig: {},
            extra_args: {},
            extra_init: {}
        );
    };

    // Header parameter (includes convert_underscores)
    ($struct_name:ident, $py_name:literal, header) => {
        $crate::define_param_internal!(
            $struct_name,
            $py_name,
            extra_fields: {
                pub convert_underscores: bool,
            },
            extra_sig: {
                convert_underscores = true,
            },
            extra_args: {
                convert_underscores: bool,
            },
            extra_init: {
                convert_underscores,
            }
        );
    };

    // Body parameter (includes embed and media_type)
    ($struct_name:ident, $py_name:literal, body) => {
        $crate::define_param_internal!(
            $struct_name,
            $py_name,
            extra_fields: {
                pub embed: Option<bool>,
                pub media_type: String,
            },
            extra_sig: {
                embed = None,
                media_type = "application/json".to_string(),
            },
            extra_args: {
                embed: Option<bool>,
                media_type: String,
            },
            extra_init: {
                embed,
                media_type,
            }
        );
    };

    // Media type parameter (Form, File)
    ($struct_name:ident, $py_name:literal, media: $default_media:literal) => {
        $crate::define_param_internal!(
            $struct_name,
            $py_name,
            extra_fields: {
                pub media_type: String,
            },
            extra_sig: {
                media_type = $default_media.to_string(),
            },
            extra_args: {
                media_type: String,
            },
            extra_init: {
                media_type,
            }
        );
    };
}

/// Defines an API key security class (e.g., APIKeyHeader, APIKeyQuery).
/// Specifies the extraction scheme for the API key in requests.
#[macro_export]
macro_rules! define_api_key_security {
    ($struct_name:ident, $py_name:literal) => {
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
            fn new(
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
        }
    };
}

/// Caches a Python module or attribute globally behind a LazyPyLock.
/// Helps avoid repeated import calls to Python modules during hot execution paths.
#[macro_export]
macro_rules! cached_py_import {
    // Module
    ($vis:vis $name:ident, $module:literal) => {
        $vis static $name: $crate::utils::LazyPyModule = $crate::utils::LazyPyModule::new($module);
    };
    // Attribute
    ($vis:vis $name:ident, $module:literal, $attr:literal) => {
        $vis static $name: $crate::utils::LazyPyAttr = $crate::utils::LazyPyAttr::new($module, $attr);
    };
    // Nested attribute
    ($vis:vis $name:ident, $module:literal, $parent:literal, $child:literal) => {
        $vis static $name: $crate::utils::LazyPyNestedAttr = $crate::utils::LazyPyNestedAttr::new($module, $parent, $child);
    };
}
