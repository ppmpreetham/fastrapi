//! The server engine of fastrapi.
//! It contains the application class, the handler executor, the exception dispatch and the HTTP server.

pub mod app;
pub mod config;
pub mod exception_dispatch;
pub mod executor;
pub mod handler_errors;
pub mod managed;
pub mod metrics;
pub mod server;
