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
/// X-Forwarded-For is only parsed when the TCP peer is an explicitly
/// configured trusted proxy (`--trusted-proxies`); loopback alone is not
/// trusted, since any local process can be a proxy and a forged header would
/// otherwise choose the rate-limit bucket and the audit log's `source_ip`.
/// From the chain, the rightmost address that is not itself a trusted proxy
/// is the client: each trusted proxy appends the address it saw to the right,
/// so attacker-supplied entries sit to the left of the selected address.
pub(crate) fn extract_ip(req: &Request, trusted_proxies: &[IpAddr]) -> Option<IpAddr> {
    let peer_ip = req
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip());

    if let Some(peer) = peer_ip
        && trusted_proxies.contains(&peer)
        && let Some(xff) = req.headers().get("x-forwarded-for")
        && let Ok(s) = xff.to_str()
    {
        for entry in s.split(',').rev() {
            if let Ok(ip) = entry.trim().parse::<IpAddr>()
                && !trusted_proxies.contains(&ip)
            {
                return Some(ip);
            }
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
