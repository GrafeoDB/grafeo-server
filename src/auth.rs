//! Bearer token authentication middleware.

use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;

use crate::error::ApiError;
use crate::state::AppState;

/// Paths exempt from authentication (monitoring/scraping).
fn is_exempt(path: &str, method: &Method) -> bool {
    if *method == Method::OPTIONS {
        return true;
    }
    matches!(path, "/health" | "/metrics")
}

/// Middleware that checks `Authorization: Bearer <token>` on non-exempt routes.
///
/// When no auth token is configured in `AppState`, all requests pass through.
pub async fn auth_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let expected = match state.auth_token() {
        Some(t) => t,
        None => return Ok(next.run(req).await),
    };

    if is_exempt(req.uri().path(), req.method()) {
        return Ok(next.run(req).await);
    }

    let auth_header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(val) if val.starts_with("Bearer ") && &val[7..] == expected => {
            Ok(next.run(req).await)
        }
        _ => Err(ApiError::Unauthorized),
    }
}
