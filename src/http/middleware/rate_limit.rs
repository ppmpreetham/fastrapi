use axum_governor::GovernorLayer;
use axum_governor::{GovernorConfigBuilder, Quota, extractor::PeerIp};
use std::net::IpAddr;
use std::num::NonZeroU32;

// TODO: add this layer to middlewares (.layer(rate_limit(param));)
pub fn rate_limit(rps: u32) -> GovernorLayer<IpAddr> {
    GovernorLayer::new(
        GovernorConfigBuilder::default()
            .with_extractor(PeerIp::default())
            .expect_connect_info()
            .quota_default(Quota::requests_per_second(NonZeroU32::new(rps).unwrap()))
            .finish()
            .unwrap(),
    )
}
