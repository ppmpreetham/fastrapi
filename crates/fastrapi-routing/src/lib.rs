//! The `fastrapi-routing` crate contains the route model, the parameter analysis, the dependency system, and the request validation for FastrAPI.
//! It also contains the Python classes for parameters, dependencies, and security schemes.
//! The main `fastrapi` crate uses these parts to register and run routes.

#![allow(clippy::result_large_err)]
#![allow(clippy::type_complexity)]
#![allow(clippy::too_many_arguments)]

mod macros;

pub mod analysis;
pub mod annotations;
pub mod dependencies;
pub mod kernel_validation;
pub mod params;
pub mod pydantic;
pub mod request_data;
pub mod route;
pub mod security;
