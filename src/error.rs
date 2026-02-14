//! HTTP error types wrapping `ServiceError`.
//!
//! `ApiError` is a newtype around `grafeo_service::error::ServiceError` that
//! adds HTTP-specific `IntoResponse` conversion. Service-layer code returns
//! `ServiceError`; the `?` operator converts automatically via `From`.

use grafeo_service::error::ServiceError;

/// HTTP-layer error wrapping `ServiceError`.
///
/// Provides `IntoResponse` (gated behind `http` feature) for JSON error bodies.
/// Service errors propagate automatically via `From<ServiceError>`.
#[derive(Debug)]
pub struct ApiError(pub ServiceError);

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for ApiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

impl From<ServiceError> for ApiError {
    fn from(e: ServiceError) -> Self {
        Self(e)
    }
}

// --- Convenience constructors (mirror ServiceError variants) ---

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self(ServiceError::BadRequest(msg.into()))
    }

    pub fn session_not_found() -> Self {
        Self(ServiceError::SessionNotFound)
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self(ServiceError::NotFound(msg.into()))
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self(ServiceError::Conflict(msg.into()))
    }

    pub fn timeout() -> Self {
        Self(ServiceError::Timeout)
    }

    pub fn unauthorized() -> Self {
        Self(ServiceError::Unauthorized)
    }

    pub fn too_many_requests() -> Self {
        Self(ServiceError::TooManyRequests)
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self(ServiceError::Internal(msg.into()))
    }
}

// ---------------------------------------------------------------------------
// HTTP response conversion (feature-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "http")]
mod http_impl {
    use super::ApiError;
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use grafeo_service::error::ServiceError;
    use serde::Serialize;
    use utoipa::ToSchema;

    #[derive(Serialize, ToSchema)]
    pub struct ErrorBody {
        /// Error code (e.g. "bad_request", "session_not_found", "internal_error").
        pub(crate) error: String,
        /// Human-readable error detail, if available.
        pub(crate) detail: Option<String>,
    }

    impl IntoResponse for ApiError {
        fn into_response(self) -> Response {
            let (status, error, detail) = match &self.0 {
                ServiceError::BadRequest(msg) => {
                    (StatusCode::BAD_REQUEST, "bad_request", Some(msg.clone()))
                }
                ServiceError::SessionNotFound => (StatusCode::NOT_FOUND, "session_not_found", None),
                ServiceError::NotFound(msg) => {
                    (StatusCode::NOT_FOUND, "not_found", Some(msg.clone()))
                }
                ServiceError::Conflict(msg) => {
                    (StatusCode::CONFLICT, "conflict", Some(msg.clone()))
                }
                ServiceError::Timeout => (StatusCode::REQUEST_TIMEOUT, "timeout", None),
                ServiceError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized", None),
                ServiceError::TooManyRequests => {
                    (StatusCode::TOO_MANY_REQUESTS, "too_many_requests", None)
                }
                ServiceError::Internal(msg) => {
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
}

#[cfg(feature = "http")]
pub use http_impl::ErrorBody;
