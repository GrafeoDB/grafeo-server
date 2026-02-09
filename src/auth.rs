//! Authentication middleware: Bearer token, API key, and HTTP Basic auth.
//!
//! All credential comparisons use constant-time equality (`subtle::ConstantTimeEq`)
//! to prevent timing side-channel attacks.

use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine as _;
use subtle::ConstantTimeEq;

use crate::error::ApiError;
use crate::state::AppState;

/// Paths exempt from authentication (monitoring/scraping).
fn is_exempt(path: &str, method: &Method) -> bool {
    if *method == Method::OPTIONS {
        return true;
    }
    matches!(path, "/health" | "/metrics")
}

/// Constant-time comparison of two byte slices.
///
/// Returns `true` only when both slices have the same length AND identical contents.
/// The length comparison itself leaks timing information (unavoidable), but the
/// content comparison is constant-time regardless of where bytes differ.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.ct_eq(b).into()
}

/// Checks if the request carries a valid Bearer token.
fn check_bearer(auth_header: Option<&str>, expected: &str) -> bool {
    auth_header
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|token| ct_eq(token.as_bytes(), expected.as_bytes()))
}

/// Checks if the request carries a valid API key via `X-API-Key` header.
fn check_api_key(req: &Request, expected: &str) -> bool {
    req.headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|key| ct_eq(key.as_bytes(), expected.as_bytes()))
}

/// Checks if the request carries valid HTTP Basic credentials.
fn check_basic_auth(auth_header: Option<&str>, expected_user: &str, expected_pass: &str) -> bool {
    let encoded = match auth_header.and_then(|v| v.strip_prefix("Basic ")) {
        Some(e) => e,
        None => return false,
    };

    let decoded = match base64::engine::general_purpose::STANDARD.decode(encoded) {
        Ok(d) => d,
        Err(_) => return false,
    };

    let decoded_str = match std::str::from_utf8(&decoded) {
        Ok(s) => s,
        Err(_) => return false,
    };

    let (user, pass) = match decoded_str.split_once(':') {
        Some(pair) => pair,
        None => return false,
    };

    ct_eq(user.as_bytes(), expected_user.as_bytes())
        && ct_eq(pass.as_bytes(), expected_pass.as_bytes())
}

/// Middleware that authenticates requests using any configured method.
///
/// Supports three credential mechanisms (checked in order):
///   1. `Authorization: Bearer <token>` — compared against `--auth-token`
///   2. `X-API-Key: <token>` — compared against `--auth-token`
///   3. `Authorization: Basic <base64(user:pass)>` — compared against `--auth-user`/`--auth-password`
///
/// When no authentication is configured, all requests pass through.
/// `/health` and `/metrics` are always exempt.
pub async fn auth_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if !state.has_auth() {
        return Ok(next.run(req).await);
    }

    if is_exempt(req.uri().path(), req.method()) {
        return Ok(next.run(req).await);
    }

    let auth_header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let auth_header_ref = auth_header.as_deref();

    // Try token-based auth (Bearer or API key)
    if let Some(expected) = state.auth_token()
        && (check_bearer(auth_header_ref, expected) || check_api_key(&req, expected))
    {
        return Ok(next.run(req).await);
    }

    // Try HTTP Basic auth
    if let Some((expected_user, expected_pass)) = state.basic_auth()
        && check_basic_auth(auth_header_ref, expected_user, expected_pass)
    {
        return Ok(next.run(req).await);
    }

    Err(ApiError::Unauthorized)
}
