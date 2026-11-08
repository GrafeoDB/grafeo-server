//! Bridges `grafeo_service::auth::AuthProviderTrait` to `boltr::server::AuthValidator`.
//!
//! Uses a `PendingAuth` map to pass full `TokenInfo` from the validator
//! to the backend. The validator stores `TokenInfo` under a UUID nonce
//! and returns that nonce as `AuthInfo.principal`. The backend pops it
//! via `set_session_auth`.

use std::sync::Arc;

use boltr::error::BoltError;
use boltr::server::{AuthCredentials, AuthInfo, AuthValidator};
use dashmap::DashMap;
use grafeo_service::auth::{AuthProviderTrait, TokenInfo};
use uuid::Uuid;

/// Shared map for passing `TokenInfo` from the validator to the backend.
/// Each entry includes an [`Instant`](std::time::Instant) for TTL-based cleanup.
pub(crate) type PendingAuth = Arc<DashMap<String, (TokenInfo, std::time::Instant)>>;

pub(crate) struct BoltrAuthValidator {
    provider: Arc<dyn AuthProviderTrait>,
    pub(crate) pending: PendingAuth,
    audit: Option<Arc<grafeo_service::audit::AuditLogger>>,
}

impl BoltrAuthValidator {
    pub fn new(
        provider: Arc<dyn AuthProviderTrait>,
        pending: PendingAuth,
        audit: Option<Arc<grafeo_service::audit::AuditLogger>>,
    ) -> Self {
        Self {
            provider,
            pending,
            audit,
        }
    }

    fn fail(&self, message: impl Into<String>) -> BoltError {
        let message = message.into();
        if let Some(audit) = &self.audit {
            audit.log(
                grafeo_service::audit::AuditEvent::new("auth_failure")
                    .transport("bolt")
                    .detail(&message),
            );
        }
        BoltError::Authentication(message)
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
                    .ok_or_else(|| self.fail("invalid bearer token"))?
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
                    return Err(self.fail("invalid credentials"));
                }
            }
            "none" => {
                return Err(self.fail("authentication required"));
            }
            other => {
                return Err(self.fail(format!("unsupported auth scheme: {other}")));
            }
        };

        let nonce = Uuid::new_v4().to_string();
        self.pending
            .insert(nonce.clone(), (token_info, std::time::Instant::now()));

        Ok(AuthInfo {
            principal: nonce,
            credentials_expired: false,
        })
    }
}
