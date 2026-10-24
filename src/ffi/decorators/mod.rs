//! The `APIRouter` Python class and its route decorators.
//! This module also flattens the router tree into a list of routes.

mod init;
mod requests;
mod tree;

pub(crate) use init::API_ROUTE_OPTIONS;
pub use tree::flatten_router;
mod router;

pub(crate) use router::collect_router_routes;
pub use router::{PyAPIRouter, SubRouterMount};
