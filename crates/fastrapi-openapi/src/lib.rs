//! Generation of the OpenAPI document for a FastrAPI application.
//! This crate builds the document from the routes, the Pydantic models, and the security schemes.

pub mod document;
pub mod paths;
pub mod schema;
pub mod spec;
