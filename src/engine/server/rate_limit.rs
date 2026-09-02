use Ordering::SeqCst;
use axum::extract::{ConnectInfo, Request};
use papaya::HashMap;
use std::{
    net::{IpAddr, SocketAddr},
    sync::OnceLock,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub(crate) struct RateLimitKey {
    handler: usize,
    ip: Option<IpAddr>,
}

pub(crate) struct RateLimitWindow {
    // u64 [32-bit timestamp seconds | 32-bit count]
    data: AtomicU64,
}

impl RateLimitWindow {
    fn is_live(&self, now_secs: u32) -> bool {
        (self.data.load(SeqCst) >> 32) as u32 >= now_secs
    }
}

pub(crate) static RATE_LIMITS: OnceLock<HashMap<RateLimitKey, RateLimitWindow>> = OnceLock::new();
pub(crate) static APP_START: OnceLock<Instant> = OnceLock::new();

static LAST_SWEEP: AtomicU64 = AtomicU64::new(0);

pub(crate) fn is_rate_limited(req: &Request, handler: usize, limit: u32) -> bool {
    if limit == 0 {
        return true;
    }

    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let key = RateLimitKey { handler, ip };

    let limits = RATE_LIMITS.get_or_init(HashMap::new);
    let start_time = APP_START.get_or_init(Instant::now);

    let now_secs = Instant::now().duration_since(*start_time).as_secs() as u32;

    let pinned = limits.pin();
    if LAST_SWEEP.swap(now_secs as u64, SeqCst) != now_secs as u64 {
        pinned.retain(|_, window| window.is_live(now_secs));
    }

    let bucket = pinned.get_or_insert_with(key, || RateLimitWindow {
        data: AtomicU64::new((now_secs as u64) << 32),
    });

    let result = bucket.data.fetch_update(SeqCst, SeqCst, |val| {
        let window_time = (val >> 32) as u32;
        let count = (val & 0xFFFFFFFF) as u32;

        if now_secs != window_time {
            Some(((now_secs as u64) << 32) | 1)
        } else if count >= limit {
            None // rate limited
        } else {
            Some(((now_secs as u64) << 32) | (count + 1) as u64)
        }
    });

    result.is_err()
}
