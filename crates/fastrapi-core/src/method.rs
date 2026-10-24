//! The HTTP methods that FastrAPI routes support.
//! `HttpMethod` gives each method a small index, so the router can keep one table for each method.

use axum::http::Method;
use strum::{AsRefStr, Display, EnumCount, EnumIter, EnumString};

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, AsRefStr, Display, EnumString, EnumCount, EnumIter,
)]
#[repr(u8)]
pub enum HttpMethod {
    GET = 0,
    POST = 1,
    PUT = 2,
    DELETE = 3,
    PATCH = 4,
    OPTIONS = 5,
    HEAD = 6,
}

impl TryFrom<&Method> for HttpMethod {
    type Error = ();

    fn try_from(method: &Method) -> Result<Self, Self::Error> {
        Ok(match *method {
            Method::GET => Self::GET,
            Method::POST => Self::POST,
            Method::PUT => Self::PUT,
            Method::DELETE => Self::DELETE,
            Method::PATCH => Self::PATCH,
            Method::OPTIONS => Self::OPTIONS,
            Method::HEAD => Self::HEAD,
            _ => return Err(()),
        })
    }
}

pub const HTTP_METHOD_COUNT: usize = HttpMethod::COUNT;
