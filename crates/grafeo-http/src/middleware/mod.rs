//! HTTP middleware: rate limiting, request ID tracking, authentication.

#[cfg(feature = "auth")]
pub mod auth;
pub mod rate_limit;
pub mod request_id;
