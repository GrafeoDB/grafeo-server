//! HTTP error types wrapping `ServiceError`.
//!
//! `ApiError` is a newtype around `grafeo_service::error::ServiceError` that
//! adds HTTP-specific `IntoResponse` conversion. Service-layer code returns
//! `ServiceError`; the `?` operator converts automatically via `From`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use grafeo_service::error::ServiceError;
use serde::Serialize;
use utoipa::ToSchema;

/// HTTP-layer error wrapping `ServiceError`.
///
/// Provides `IntoResponse` for JSON error bodies.
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
// HTTP response conversion
// ---------------------------------------------------------------------------

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
            ServiceError::NotFound(msg) => (StatusCode::NOT_FOUND, "not_found", Some(msg.clone())),
            ServiceError::Conflict(msg) => (StatusCode::CONFLICT, "conflict", Some(msg.clone())),
            ServiceError::Timeout => (StatusCode::REQUEST_TIMEOUT, "timeout", None),
            ServiceError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized", None),
            ServiceError::TooManyRequests => {
                (StatusCode::TOO_MANY_REQUESTS, "too_many_requests", None)
            }
            ServiceError::ReadOnly => (
                StatusCode::FORBIDDEN,
                "read_only",
                Some("server is in read-only mode".to_string()),
            ),
            ServiceError::Unavailable(msg) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                Some(msg.clone()),
            ),
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

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    use super::ApiError;

    async fn parse_response(err: ApiError) -> (StatusCode, serde_json::Value) {
        let resp = err.into_response();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    #[tokio::test]
    async fn bad_request_maps_to_400() {
        let (status, body) = parse_response(ApiError::bad_request("oops")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "bad_request");
        assert_eq!(body["detail"], "oops");
    }

    #[tokio::test]
    async fn session_not_found_maps_to_404() {
        let (status, body) = parse_response(ApiError::session_not_found()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "session_not_found");
        assert!(body["detail"].is_null());
    }

    #[tokio::test]
    async fn not_found_maps_to_404() {
        let (status, body) = parse_response(ApiError::not_found("db 'x' not found")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "not_found");
        assert_eq!(body["detail"], "db 'x' not found");
    }

    #[tokio::test]
    async fn conflict_maps_to_409() {
        let (status, body) = parse_response(ApiError::conflict("already exists")).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "conflict");
        assert_eq!(body["detail"], "already exists");
    }

    #[tokio::test]
    async fn timeout_maps_to_408() {
        let (status, body) = parse_response(ApiError::timeout()).await;
        assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
        assert_eq!(body["error"], "timeout");
        assert!(body["detail"].is_null());
    }

    #[tokio::test]
    async fn unauthorized_maps_to_401() {
        let (status, body) = parse_response(ApiError::unauthorized()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "unauthorized");
        assert!(body["detail"].is_null());
    }

    #[tokio::test]
    async fn too_many_requests_maps_to_429() {
        let (status, body) = parse_response(ApiError::too_many_requests()).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"], "too_many_requests");
        assert!(body["detail"].is_null());
    }

    #[tokio::test]
    async fn read_only_maps_to_403() {
        use grafeo_service::error::ServiceError;
        let (status, body) = parse_response(ApiError::from(ServiceError::ReadOnly)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "read_only");
        assert_eq!(body["detail"], "server is in read-only mode");
    }

    #[tokio::test]
    async fn unavailable_maps_to_503() {
        use grafeo_service::error::ServiceError;
        let (status, body) = parse_response(ApiError::from(ServiceError::Unavailable(
            "restoring".into(),
        )))
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "unavailable");
        assert_eq!(body["detail"], "restoring");
    }

    #[tokio::test]
    async fn internal_maps_to_500() {
        let (status, body) = parse_response(ApiError::internal("disk full")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"], "internal_error");
        assert_eq!(body["detail"], "disk full");
    }

    #[tokio::test]
    async fn from_service_error_conversion() {
        use grafeo_service::error::ServiceError;
        let api_err = ApiError::from(ServiceError::Timeout);
        let (status, _) = parse_response(api_err).await;
        assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
    }

    #[test]
    fn display_delegates_to_inner() {
        let err = ApiError::bad_request("test msg");
        assert!(err.to_string().contains("test msg"));
    }

    #[test]
    fn error_source_is_service_error() {
        use std::error::Error;
        let err = ApiError::not_found("x");
        assert!(err.source().is_some());
    }
}
