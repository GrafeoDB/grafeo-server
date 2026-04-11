//! Bridges `grafeo_service::auth::AuthProvider` to `boltr::server::AuthValidator`.
//!
//! Uses a `PendingAuth` map to pass full `TokenInfo` from the validator
//! to the backend. The validator stores `TokenInfo` under a UUID nonce
//! and returns that nonce as `AuthInfo.principal`. The backend pops it
//! via `set_session_auth`.

use std::sync::Arc;

use boltr::error::BoltError;
use boltr::server::{AuthCredentials, AuthInfo, AuthValidator};
use dashmap::DashMap;
use grafeo_service::auth::{AuthProvider, TokenInfo};
use uuid::Uuid;

/// Shared map for passing `TokenInfo` from the validator to the backend.
pub(crate) type PendingAuth = Arc<DashMap<String, TokenInfo>>;

pub(crate) struct BoltrAuthValidator {
    provider: AuthProvider,
    pub(crate) pending: PendingAuth,
}

impl BoltrAuthValidator {
    pub fn new(provider: AuthProvider, pending: PendingAuth) -> Self {
        Self { provider, pending }
    }
}

#[async_trait::async_trait]
impl AuthValidator for BoltrAuthValidator {
    async fn validate(&self, credentials: &AuthCredentials) -> Result<AuthInfo, BoltError> {
        let token_info = match credentials.scheme.as_str() {
            "bearer" => {
                let token = credentials.credentials.as_deref().unwrap_or("");
                self.provider
                    .check_bearer(token)
                    .ok_or_else(|| BoltError::Authentication("invalid bearer token".into()))?
            }
            "basic" => {
                let user = credentials.principal.as_deref().unwrap_or("");
                let pass = credentials.credentials.as_deref().unwrap_or("");
                if self.provider.check_basic(user, pass) {
                    // Basic auth grants admin scope (matches HTTP behavior).
                    TokenInfo {
                        id: "_basic".to_string(),
                        name: user.to_string(),
                        scope: grafeo_service::auth::TokenScope::default(),
                    }
                } else {
                    return Err(BoltError::Authentication("invalid credentials".into()));
                }
            }
            "none" => {
                return Err(BoltError::Authentication("authentication required".into()));
            }
            other => {
                return Err(BoltError::Authentication(format!(
                    "unsupported auth scheme: {other}"
                )));
            }
        };

        let nonce = Uuid::new_v4().to_string();
        self.pending.insert(nonce.clone(), token_info);

        Ok(AuthInfo {
            principal: nonce,
            credentials_expired: false,
        })
    }
}
