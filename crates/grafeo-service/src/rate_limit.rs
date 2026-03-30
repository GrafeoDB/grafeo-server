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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, last))
    }

    // -----------------------------------------------------------------------
    // Disabled limiter (max_requests = 0)
    // -----------------------------------------------------------------------

    #[test]
    fn disabled_limiter_always_allows() {
        let rl = RateLimiter::new(0, Duration::from_secs(60));
        assert!(!rl.is_enabled());
        for _ in 0..100 {
            assert!(rl.check(ip(1)));
        }
    }

    // -----------------------------------------------------------------------
    // Basic window behavior
    // -----------------------------------------------------------------------

    #[test]
    fn allows_up_to_max_requests() {
        let rl = RateLimiter::new(3, Duration::from_secs(60));
        assert!(rl.is_enabled());
        assert!(rl.check(ip(1))); // 1
        assert!(rl.check(ip(1))); // 2
        assert!(rl.check(ip(1))); // 3
        assert!(!rl.check(ip(1))); // blocked
    }

    #[test]
    fn single_request_limit() {
        let rl = RateLimiter::new(1, Duration::from_secs(60));
        assert!(rl.check(ip(1)));
        assert!(!rl.check(ip(1)));
    }

    // -----------------------------------------------------------------------
    // Per-IP isolation
    // -----------------------------------------------------------------------

    #[test]
    fn different_ips_have_separate_counters() {
        let rl = RateLimiter::new(2, Duration::from_secs(60));
        assert!(rl.check(ip(1))); // ip1: 1
        assert!(rl.check(ip(1))); // ip1: 2
        assert!(!rl.check(ip(1))); // ip1: blocked

        // ip(2) should still be allowed.
        assert!(rl.check(ip(2))); // ip2: 1
        assert!(rl.check(ip(2))); // ip2: 2
        assert!(!rl.check(ip(2))); // ip2: blocked
    }

    // -----------------------------------------------------------------------
    // Window expiration
    // -----------------------------------------------------------------------

    #[test]
    fn window_expiration_resets_counter() {
        // Use a very short window so it expires during the test.
        let rl = RateLimiter::new(2, Duration::from_millis(5));
        assert!(rl.check(ip(1)));
        assert!(rl.check(ip(1)));
        assert!(!rl.check(ip(1))); // blocked

        // Wait for window to expire.
        std::thread::sleep(Duration::from_millis(10));

        // Should be allowed again after window reset.
        assert!(rl.check(ip(1)));
    }

    // -----------------------------------------------------------------------
    // Cleanup
    // -----------------------------------------------------------------------

    #[test]
    fn cleanup_removes_expired_entries() {
        let rl = RateLimiter::new(5, Duration::from_millis(5));
        rl.check(ip(1));
        rl.check(ip(2));

        // Wait for window to expire.
        std::thread::sleep(Duration::from_millis(10));

        rl.cleanup();

        // After cleanup, the internal map should be empty.
        // Verify by checking that a new request is allowed (counter reset).
        assert!(rl.check(ip(1)));
        assert!(rl.check(ip(1)));
    }

    #[test]
    fn cleanup_keeps_fresh_entries() {
        let rl = RateLimiter::new(2, Duration::from_secs(60));
        rl.check(ip(1));
        rl.check(ip(1));

        rl.cleanup();

        // Entry should still be there: next check should be blocked.
        assert!(!rl.check(ip(1)));
    }

    // -----------------------------------------------------------------------
    // Clone behavior (shared state via Arc)
    // -----------------------------------------------------------------------

    #[test]
    fn cloned_limiter_shares_state() {
        let rl = RateLimiter::new(2, Duration::from_secs(60));
        let rl2 = rl.clone();
        assert!(rl.check(ip(1)));
        assert!(rl2.check(ip(1)));
        assert!(!rl.check(ip(1))); // shared counter exhausted
    }
}
