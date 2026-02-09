//! Per-IP rate limiting middleware using a fixed-window counter.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use dashmap::DashMap;

use crate::error::ApiError;
use crate::state::AppState;

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

/// Extracts the client IP from the request.
fn extract_ip(req: &Request) -> Option<IpAddr> {
    // X-Forwarded-For takes priority (reverse proxy)
    if let Some(xff) = req.headers().get("x-forwarded-for")
        && let Ok(s) = xff.to_str()
        && let Some(first) = s.split(',').next()
        && let Ok(ip) = first.trim().parse::<IpAddr>()
    {
        return Some(ip);
    }

    // Fallback to ConnectInfo (direct connection)
    req.extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip())
}

/// Rate-limiting middleware. Returns 429 when the per-IP limit is exceeded.
pub async fn rate_limit_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let limiter = state.rate_limiter();
    if !limiter.is_enabled() {
        return Ok(next.run(req).await);
    }

    if let Some(ip) = extract_ip(&req)
        && !limiter.check(ip)
    {
        return Err(ApiError::TooManyRequests);
    }

    Ok(next.run(req).await)
}
