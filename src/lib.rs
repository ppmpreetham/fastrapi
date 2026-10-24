//! The `fastrapi` crate builds the Python extension module of fastrapi.
//! fastrapi is a web framework that is compatible with FastAPI. It uses pyo3, axum and tokio.
//! The crate contains the server engine, the Python bindings and the module registration.

#![allow(clippy::result_large_err)]
#![allow(clippy::type_complexity)]
#![allow(clippy::too_many_arguments)]

pub mod engine;
pub mod ffi;
pub mod macros;
mod python_module;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
