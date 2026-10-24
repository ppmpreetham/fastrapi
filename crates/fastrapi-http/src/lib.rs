//! The `fastrapi-http` crate contains the HTTP layer of fastrapi.
//! It gives the Python classes for requests, responses, WebSockets, forms, files, exceptions, and middleware.
//! These classes are compatible with FastAPI and starlette.

#![allow(clippy::result_large_err)]
#![allow(clippy::type_complexity)]
#![allow(clippy::too_many_arguments)]

mod macros;

pub mod background;
pub mod exceptions;
pub mod form;
pub mod form_data;
pub mod middleware;
pub mod request;
pub mod request_input;
pub mod response_api;
pub mod response_convert;
pub mod response_file;
pub mod response_type;
pub mod responses;
pub mod sse;
pub mod staticfiles;
pub mod status;
pub mod templating;
pub mod upload_file;
pub mod url;
pub mod websocket;
