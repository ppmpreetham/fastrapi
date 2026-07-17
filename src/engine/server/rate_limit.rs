use axum::extract::{ConnectInfo, Request};
use dashmap::DashMap;
use parking_lot::Mutex;
use std::{
    net::{IpAddr, SocketAddr},
    sync::OnceLock,
    time::{Duration, Instant},
};

#[derive(Clone, Hash, PartialEq, Eq)]
pub(crate) struct RateLimitKey {
    handler: usize,
    ip: Option<IpAddr>,
}

pub(crate) struct RateLimitWindow {
    start: Instant,
    count: u32,
}

pub(crate) static RATE_LIMITS: OnceLock<DashMap<RateLimitKey, Mutex<RateLimitWindow>>> =
    OnceLock::new();

pub(crate) fn is_rate_limited(req: &Request, handler: usize, limit: u32) -> bool {
    if limit == 0 {
        return true;
    }

    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let key = RateLimitKey { handler, ip };
    let limits = RATE_LIMITS.get_or_init(DashMap::new);
    let bucket = limits.entry(key).or_insert_with(|| {
        Mutex::new(RateLimitWindow {
            start: Instant::now(),
            count: 0,
        })
    });
    let mut window = bucket.lock();
    let now = Instant::now();

    if now.duration_since(window.start) >= Duration::from_secs(1) {
        window.start = now;
        window.count = 0;
    }

    if window.count >= limit {
        return true;
    }

    window.count += 1;
    false
}
