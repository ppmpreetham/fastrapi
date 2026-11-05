//! The HTTP methods that FastrAPI routes support.
//! `HttpMethod` gives each method a small index, so the router can keep one table for each method.

use axum::http::{HeaderValue, Method};
use std::sync::LazyLock;
use strum::{AsRefStr, Display, EnumCount, EnumIter, EnumString, IntoEnumIterator};

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
    TRACE = 7,
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
            Method::TRACE => Self::TRACE,
            _ => return Err(()),
        })
    }
}

pub const HTTP_METHOD_COUNT: usize = HttpMethod::COUNT;

impl HttpMethod {
    #[must_use]
    pub const fn bit(self) -> u8 {
        1 << self as u8
    }
}

static ALLOW_HEADERS: LazyLock<Box<[HeaderValue]>> = LazyLock::new(|| {
    (0..=u8::MAX)
        .map(|mask| {
            let joined = HttpMethod::iter()
                .filter(|method| mask & method.bit() != 0)
                .map(|method| method.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            HeaderValue::from_str(&joined).unwrap_or_else(|_| HeaderValue::from_static(""))
        })
        .collect()
});

#[must_use]
pub fn allow_header(mask: u8) -> HeaderValue {
    ALLOW_HEADERS[usize::from(mask)].clone()
}
