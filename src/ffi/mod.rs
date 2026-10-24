//! The Python bindings (FFI) of fastrapi.
//! They contain the router decorators, the encoders, the OpenAPI functions and the routing classes.

pub(crate) mod compat;
pub mod decorators;
pub mod encoders;
pub mod openapi;
pub mod routing;
