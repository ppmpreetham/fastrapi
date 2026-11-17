//! The `APIRouter` Python class and its route decorators.
//! This module also flattens the router tree into a list of routes.

mod init;
mod requests;
pub(crate) mod tree;

pub(crate) use init::{add_api_route_kwargs, ensure_route_options, ensure_routed_path, tag_name};
pub use tree::flatten_router;
mod router;

pub use router::{PyAPIRouter, SubRouterMount};
pub(crate) use router::{
    api_router_type, collect_router_routes, new_api_router, warn_on_event_deprecated,
};
