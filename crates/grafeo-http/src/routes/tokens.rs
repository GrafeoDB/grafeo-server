//! Token management endpoints — CRUD for API keys.

use axum::extract::{Json, Path, State};
use axum::response::IntoResponse;

use crate::error::ApiError;
use crate::middleware::auth_context::AuthContext;
use crate::state::AppState;

use grafeo_service::types;

/// Create a new API token.
///
/// Returns the plaintext token once. It cannot be retrieved again.
/// Requires admin role.
#[utoipa::path(
    post,
    path = "/admin/tokens",
    request_body = types::CreateTokenRequest,
    responses(
        (status = 200, description = "Token created", body = types::TokenResponse),
        (status = 403, description = "Admin access required", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn create_token(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(req): Json<types::CreateTokenRequest>,
) -> Result<Json<types::TokenResponse>, ApiError> {
    auth.check_admin()?;

    let store = state.auth().and_then(|a| a.token_store()).ok_or_else(|| {
        grafeo_service::error::ServiceError::BadRequest(
            "token management not configured".to_string(),
        )
    })?;

    let (record, plaintext) = grafeo_service::token_service::TokenService::create_token(
        store,
        req.name,
        req.scope,
        req.expires_in,
    )
    .map_err(ApiError::from)?;

    Ok(Json(types::TokenResponse {
        id: record.id,
        name: record.name,
        scope: types::TokenScopeRequest {
            role: grafeo_service::auth::role_to_str(record.scope.role).to_string(),
            databases: record.scope.databases,
        },
        created_at: record.created_at,
        expires_at: record.expires_at,
        token: Some(plaintext),
    }))
}

/// List all API tokens.
///
/// Returns token metadata only (no plaintext values).
/// Requires admin role.
#[utoipa::path(
    get,
    path = "/admin/tokens",
    responses(
        (status = 200, description = "Token list", body = Vec<types::TokenResponse>),
        (status = 403, description = "Admin access required", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn list_tokens(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<Json<Vec<types::TokenResponse>>, ApiError> {
    auth.check_admin()?;

    let store = state.auth().and_then(|a| a.token_store()).ok_or_else(|| {
        grafeo_service::error::ServiceError::BadRequest(
            "token management not configured".to_string(),
        )
    })?;

    let tokens = grafeo_service::token_service::TokenService::list_tokens(store);
    Ok(Json(tokens))
}

/// Get a single token by ID.
///
/// Requires admin role.
#[utoipa::path(
    get,
    path = "/admin/tokens/{id}",
    params(
        ("id" = String, Path, description = "Token ID"),
    ),
    responses(
        (status = 200, description = "Token details", body = types::TokenResponse),
        (status = 403, description = "Admin access required", body = crate::error::ErrorBody),
        (status = 404, description = "Token not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn get_token(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(id): Path<String>,
) -> Result<Json<types::TokenResponse>, ApiError> {
    auth.check_admin()?;

    let store = state.auth().and_then(|a| a.token_store()).ok_or_else(|| {
        grafeo_service::error::ServiceError::BadRequest(
            "token management not configured".to_string(),
        )
    })?;

    let token = grafeo_service::token_service::TokenService::get_token(store, &id)
        .map_err(ApiError::from)?;
    Ok(Json(token))
}

/// Revoke (delete) a token.
///
/// Takes effect immediately — the next request with this token will be rejected.
/// Requires admin role.
#[utoipa::path(
    delete,
    path = "/admin/tokens/{id}",
    params(
        ("id" = String, Path, description = "Token ID"),
    ),
    responses(
        (status = 200, description = "Token revoked"),
        (status = 403, description = "Admin access required", body = crate::error::ErrorBody),
        (status = 404, description = "Token not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn delete_token(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    auth.check_admin()?;

    let store = state.auth().and_then(|a| a.token_store()).ok_or_else(|| {
        grafeo_service::error::ServiceError::BadRequest(
            "token management not configured".to_string(),
        )
    })?;

    grafeo_service::token_service::TokenService::delete_token(store, &id)
        .map_err(ApiError::from)?;

    Ok(Json(serde_json::json!({ "deleted": true })))
}
