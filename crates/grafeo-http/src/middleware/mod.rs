//! HTTP middleware: rate limiting, request ID tracking, authentication.

#[cfg(feature = "auth")]
pub mod auth;
pub mod auth_context;
pub mod rate_limit;
#[cfg(feature = "replication")]
pub mod replica_guard;
pub mod request_id;
#[cfg(feature = "auth")]
pub mod studio_auth;
