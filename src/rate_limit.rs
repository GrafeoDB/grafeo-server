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
        return Err(ApiError::too_many_requests());
    }

    Ok(next.run(req).await)
}
