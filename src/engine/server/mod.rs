//! The HTTP server of fastrapi, which uses axum and tokio.
//! It contains the request dispatch, the routes, the middleware stack, the lifespan and the WebSocket support.

pub mod dispatch;
pub mod files;
pub mod lifecycle;
pub(crate) mod middleware_stack;
pub mod payload;
pub mod routes;
pub mod serve;
pub mod websocket;
