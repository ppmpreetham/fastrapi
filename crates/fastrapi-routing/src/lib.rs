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
