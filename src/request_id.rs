//! Request ID middleware: propagates or generates a unique ID per request.

use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument;
use uuid::Uuid;

static X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Ensures every request carries an `X-Request-Id` header.
///
/// If the incoming request already has the header, it is preserved.
/// Otherwise a new UUID v4 is generated. The ID is:
/// - injected into the request headers (for downstream extractors),
/// - set on the response headers (for client correlation),
/// - attached to a tracing span so all logs include `request_id`.
pub async fn request_id_middleware(mut req: Request, next: Next) -> Response {
    let request_id = req
        .headers()
        .get(&X_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .map_or_else(|| Uuid::new_v4().to_string(), String::from);

    if let Ok(val) = HeaderValue::from_str(&request_id) {
        req.headers_mut().insert(X_REQUEST_ID.clone(), val);
    }

    let span = tracing::info_span!("request", request_id = %request_id);
    let mut response = next.run(req).instrument(span).await;

    if let Ok(val) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(X_REQUEST_ID.clone(), val);
    }

    response
}
