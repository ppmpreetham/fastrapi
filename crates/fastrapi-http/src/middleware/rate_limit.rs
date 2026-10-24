//! This module makes a rate limit layer.
//! The layer limits the number of requests that each client IP address can send in one second.

use axum_governor::extractor::PeerIp;
use axum_governor::{GovernorConfigBuilder, GovernorLayer, Quota};
use std::net::IpAddr;
use std::num::NonZeroU32;

// TODO: add this layer to middlewares (.layer(rate_limit(param));)
#[must_use]
pub fn rate_limit(rps: NonZeroU32) -> GovernorLayer<IpAddr> {
    GovernorLayer::new(
        GovernorConfigBuilder::default()
            .with_extractor(PeerIp::default())
            .expect_connect_info()
            .quota_default(Quota::requests_per_second(rps))
            .finish()
            .expect("a non-zero quota always yields a valid governor config"),
    )
}
