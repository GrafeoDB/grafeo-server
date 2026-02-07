//! Error types for the HTTP API layer.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

/// API error that serializes to JSON.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
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

    /// Missing or invalid authentication token.
    #[error("unauthorized")]
    Unauthorized,

    /// Internal server error.
    #[error("internal error: {0}")]
    Internal(String),
}

#[derive(Serialize, ToSchema)]
pub struct ErrorBody {
    /// Error code (e.g. "bad_request", "session_not_found", "internal_error").
    error: String,
    /// Human-readable error detail, if available.
    detail: Option<String>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error, detail) = match &self {
            ApiError::BadRequest(msg) => {
                (StatusCode::BAD_REQUEST, "bad_request", Some(msg.clone()))
            }
            ApiError::SessionNotFound => (StatusCode::NOT_FOUND, "session_not_found", None),
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, "not_found", Some(msg.clone())),
            ApiError::Conflict(msg) => (StatusCode::CONFLICT, "conflict", Some(msg.clone())),
            ApiError::Timeout => (StatusCode::REQUEST_TIMEOUT, "timeout", None),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized", None),
            ApiError::Internal(msg) => {
                tracing::error!(%msg, "internal server error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    Some(msg.clone()),
                )
            }
        };

        let body = ErrorBody {
            error: error.to_string(),
            detail,
        };

        (status, axum::Json(body)).into_response()
    }
}
