crate::define_api_key_security!(APIKeyHeader, "APIKeyHeader", header);
crate::define_api_key_security!(APIKeyQuery, "APIKeyQuery", query);
crate::define_api_key_security!(APIKeyCookie, "APIKeyCookie", cookie);

#[macro_export]
macro_rules! define_api_key_security {
    ($struct_name:ident, $py_name:literal, $source:ident) => {
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

            fn __call__(
                &self,
                request: &pyo3::Bound<'_, pyo3::types::PyAny>,
            ) -> pyo3::PyResult<String> {
                $crate::routing::security::callable::api_key_call(
                    stringify!($source),
                    &self.name,
                    self.auto_error,
                    request,
                )
            }
        }
    };
}
