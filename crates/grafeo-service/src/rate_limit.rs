//! Per-IP rate limiting with a fixed-window counter.
//!
//! Transport-agnostic core. Each transport crate wires its own middleware
//! to extract client IPs and call `check()`.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;

/// In-memory per-IP rate limiter with fixed-window counters.
#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<RateLimiterInner>,
}

struct RateLimiterInner {
    max_requests: u64,
    window: Duration,
    counters: DashMap<IpAddr, (u64, Instant)>,
}

impl RateLimiter {
    /// Creates a new rate limiter. `max_requests = 0` means disabled.
    pub fn new(max_requests: u64, window: Duration) -> Self {
        Self {
            inner: Arc::new(RateLimiterInner {
                max_requests,
                window,
                counters: DashMap::new(),
            }),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.max_requests > 0
    }

    /// Returns `true` if the request is allowed, `false` if rate-limited.
    pub fn check(&self, ip: IpAddr) -> bool {
        if !self.is_enabled() {
            return true;
        }

        let mut entry = self.inner.counters.entry(ip).or_insert((0, Instant::now()));
        let (count, window_start) = entry.value_mut();

        if window_start.elapsed() > self.inner.window {
            // Window expired — reset
            *count = 1;
            *window_start = Instant::now();
            true
        } else if *count < self.inner.max_requests {
            *count += 1;
            true
        } else {
            false
        }
    }

    /// Removes entries for expired windows (background cleanup).
    pub fn cleanup(&self) {
        let window = self.inner.window;
        self.inner
            .counters
            .retain(|_, (_, start)| start.elapsed() <= window);
    }
}
