//! Service-layer error types.
//!
//! `ServiceError` is transport-agnostic. Each transport crate maps it to
//! its own wire format (HTTP status codes, GQL error codes, Bolt failures).

/// Service error shared across all transports.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// Query execution failed (bad syntax, type mismatch, etc.).
    #[error("{0}")]
    BadRequest(String),

    /// Transaction session not found or expired.
    #[error("session not found or expired")]
    SessionNotFound,

    /// Resource not found.
    #[error("{0}")]
    NotFound(String),

    /// Resource already exists.
    #[error("{0}")]
    Conflict(String),

    /// Query execution timed out.
    #[error("query execution timed out")]
    Timeout,

    /// Missing or invalid authentication.
    #[error("unauthorized")]
    Unauthorized,

    /// Rate limit exceeded.
    #[error("too many requests")]
    TooManyRequests,

    /// Internal server error.
    #[error("internal error: {0}")]
    Internal(String),
}
