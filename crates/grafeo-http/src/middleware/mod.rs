//! HTTP middleware: rate limiting, request ID tracking, authentication,
//! cross-site request refusal.

#[cfg(feature = "auth")]
pub mod auth;
pub mod auth_context;
#[cfg(feature = "auth")]
pub(crate) mod authorize;
pub mod cross_site;
pub mod rate_limit;
#[cfg(feature = "replication")]
pub mod replica_guard;
pub mod request_id;
#[cfg(feature = "auth")]
pub mod studio_auth;
