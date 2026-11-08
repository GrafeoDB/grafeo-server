//! HTTP authentication middleware.
//!
//! Credential extraction from HTTP headers lives here. Credential verification
//! is delegated to `grafeo_service::auth::AuthProvider`.
//!
//! Supports three mechanisms (checked in order):
//!   1. `Authorization: Bearer <token>` — compared against configured tokens
//!   2. `X-API-Key: <token>` — compared against configured tokens
//!   3. `Authorization: Basic <base64(user:pass)>` — compared against `--auth-user`/`--auth-password`
//!
//! On success, inserts `TokenInfo` into request extensions so downstream
//! handlers can check token scope via the `AuthContext` extractor.

use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine as _;

use grafeo_service::auth::TokenInfo;

use crate::error::ApiError;
use crate::state::AppState;

/// Paths exempt from authentication: CORS preflights, the `/health` probe,
/// and `/metrics` when the server runs with `--public-metrics`.
fn is_exempt(path: &str, method: &Method, public_metrics: bool) -> bool {
    *method == Method::OPTIONS || path == "/health" || (public_metrics && path == "/metrics")
}

/// Middleware that authenticates requests using any configured method.
///
/// When no authentication is configured, all requests pass through.
/// `/health` is always exempt, `/metrics` with `--public-metrics`.
/// On success, `TokenInfo` is inserted into request extensions.
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let auth_provider = match state.auth() {
        Some(p) => p,
        None => return Ok(next.run(req).await),
    };

    if is_exempt(req.uri().path(), req.method(), state.public_metrics()) {
        return Ok(next.run(req).await);
    }

    let auth_header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let auth_header_ref = auth_header.as_deref();

    let api_key = req.headers().get("x-api-key").and_then(|v| v.to_str().ok());

    let info = auth_header_ref
        // Bearer token
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|token| auth_provider.check_bearer(token))
        // API key header (checked against the same token set)
        .or_else(|| api_key.and_then(|key| auth_provider.check_bearer(key)))
        // HTTP Basic auth: admin, unrestricted
        .or_else(|| {
            let encoded = auth_header_ref?.strip_prefix("Basic ")?;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .ok()?;
            let (user, pass) = std::str::from_utf8(&decoded).ok()?.split_once(':')?;
            auth_provider.check_basic(user, pass).then(|| TokenInfo {
                id: "_basic".to_string(),
                name: "basic-auth".to_string(),
                scope: grafeo_service::auth::TokenScope::default(),
            })
        });

    let Some(info) = info else {
        state.audit_event(
            grafeo_service::audit::AuditEvent::new("auth_failure")
                .transport("http")
                .source_ip(super::rate_limit::extract_ip(&req, state.trusted_proxies()))
                .detail(format!("{} {}", req.method(), req.uri().path())),
        );
        return Err(ApiError::unauthorized());
    };

    if let Err(e) = super::authorize::authorize(req.method(), req.uri().path(), &info) {
        state.audit_event(
            grafeo_service::audit::AuditEvent::new("access_denied")
                .actor(Some(&info.name))
                .transport("http")
                .source_ip(super::rate_limit::extract_ip(&req, state.trusted_proxies()))
                .detail(format!("{} {}: {e}", req.method(), req.uri().path())),
        );
        return Err(e);
    }
    req.extensions_mut().insert(info);
    Ok(next.run(req).await)
}
