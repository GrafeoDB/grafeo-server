//! HTTP rate-limiting middleware.
//!
//! The core `RateLimiter` lives in `grafeo_service::rate_limit`. This module
//! provides only the axum middleware that extracts client IPs from HTTP
//! requests and delegates to the rate limiter.

use std::net::IpAddr;

use axum::extract::{ConnectInfo, Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::error::ApiError;
use crate::state::AppState;

/// Extracts the client IP from the request.
///
/// X-Forwarded-For is only trusted when the TCP peer is a known trusted proxy
/// (configured via `--trusted-proxies`). This prevents spoofing by arbitrary
/// clients sending fake XFF headers to bypass rate limiting.
fn extract_ip(req: &Request, trusted_proxies: &[IpAddr]) -> Option<IpAddr> {
    let peer_ip = req
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip());

    // Only parse X-Forwarded-For when the direct peer is a trusted proxy
    if let Some(peer) = peer_ip {
        let peer_trusted = peer.is_loopback() || trusted_proxies.contains(&peer);
        if peer_trusted
            && let Some(xff) = req.headers().get("x-forwarded-for")
            && let Ok(s) = xff.to_str()
            && let Some(first) = s.split(',').next()
            && let Ok(ip) = first.trim().parse::<IpAddr>()
        {
            return Some(ip);
        }
    }

    peer_ip
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

    if let Some(ip) = extract_ip(&req, state.trusted_proxies())
        && !limiter.check(ip)
    {
        return Err(ApiError::too_many_requests());
    }

    Ok(next.run(req).await)
}
