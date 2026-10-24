/// Internal macro used to construct parameter types.
/// It defines the Python class structure, properties, and methods for a parameter.
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
            const fn new(
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
macro_rules! define_param {
    // Base parameter (Query, Path, Cookie)
    ($struct_name:ident, $py_name:literal) => {
        $crate::macros::define_param_internal!(
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
        $crate::macros::define_param_internal!(
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
        $crate::macros::define_param_internal!(
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
        $crate::macros::define_param_internal!(
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

pub(crate) use define_param;
pub(crate) use define_param_internal;
