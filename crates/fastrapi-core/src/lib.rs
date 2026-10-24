//! Core data types for the FastrAPI web framework.
//! This crate contains the route table, HTTP methods, path tools, parameter and body types,
//! security scheme types, rate limits, and the reload supervisor.
//! It does not depend on Python, so other FastrAPI crates can use it.

pub mod body;
pub mod method;
pub mod params;
pub mod path;
pub mod rate_limit;
pub mod reload;
pub mod router;
pub mod security;
