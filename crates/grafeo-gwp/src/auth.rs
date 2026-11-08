//! GWP authentication adapter.
//!
//! Bridges `grafeo_service::auth::AuthProviderTrait` to `gwp::server::AuthValidator`,
//! mapping GWP proto credentials to the transport-agnostic auth checks.
//!
//! Uses a `PendingAuth` map to pass full `TokenInfo` from the validator
//! (which verifies credentials) to the backend (which creates sessions).
//! The validator stores `TokenInfo` under a UUID nonce and returns that
//! nonce as the `AuthInfo.principal`. The backend pops it by principal.

use std::sync::Arc;

use dashmap::DashMap;
use grafeo_service::auth::{AuthProviderTrait, TokenInfo};
use gwp::error::GqlError;
use gwp::proto;
use gwp::server::{AuthInfo, AuthValidator};
use uuid::Uuid;

/// Shared map for passing `TokenInfo` from the validator to the backend.
/// Each entry includes an [`Instant`](std::time::Instant) for TTL-based cleanup.
pub(crate) type PendingAuth = Arc<DashMap<String, (TokenInfo, std::time::Instant)>>;

/// Validates GWP handshake credentials using the shared [`AuthProviderTrait`].
pub(crate) struct GwpAuthValidator {
    provider: Arc<dyn AuthProviderTrait>,
    pub(crate) pending: PendingAuth,
    audit: Option<Arc<grafeo_service::audit::AuditLogger>>,
}

impl GwpAuthValidator {
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

    fn fail(&self, message: &str) -> GqlError {
        if let Some(audit) = &self.audit {
            audit.log(
                grafeo_service::audit::AuditEvent::new("auth_failure")
                    .transport("gwp")
                    .detail(message),
            );
        }
        GqlError::Protocol(message.to_owned())
    }
}

#[tonic::async_trait]
impl AuthValidator for GwpAuthValidator {
    async fn validate(&self, credentials: &proto::AuthCredentials) -> Result<AuthInfo, GqlError> {
        use proto::auth_credentials::Method;

        let token_info = match &credentials.method {
            Some(Method::BearerToken(token)) => self
                .provider
                .check_bearer(token)
                .ok_or_else(|| self.fail("invalid bearer token"))?,
            Some(Method::Basic(basic)) => {
                if self.provider.check_basic(&basic.username, &basic.password) {
                    // Basic auth grants admin scope (matches HTTP behavior).
                    TokenInfo {
                        id: "_basic".to_string(),
                        name: basic.username.clone(),
                        scope: grafeo_service::auth::TokenScope::default(),
                    }
                } else {
                    return Err(self.fail("invalid credentials"));
                }
            }
            None => return Err(self.fail("credentials required")),
        };

        let nonce = Uuid::new_v4().to_string();
        self.pending
            .insert(nonce.clone(), (token_info, std::time::Instant::now()));

        Ok(AuthInfo { principal: nonce })
    }
}
