//! Bridges `grafeo_service::auth::AuthProvider` to `boltr::server::AuthValidator`.

use boltr::error::BoltError;
use boltr::server::{AuthCredentials, AuthValidator};
use grafeo_service::auth::AuthProvider;

pub(crate) struct BoltrAuthValidator {
    provider: AuthProvider,
}

impl BoltrAuthValidator {
    pub fn new(provider: AuthProvider) -> Self {
        Self { provider }
    }
}

#[async_trait::async_trait]
impl AuthValidator for BoltrAuthValidator {
    async fn validate(&self, credentials: &AuthCredentials) -> Result<(), BoltError> {
        match credentials.scheme.as_str() {
            "bearer" => {
                let token = credentials.credentials.as_deref().unwrap_or("");
                if self.provider.check_bearer(token) {
                    Ok(())
                } else {
                    Err(BoltError::Authentication("invalid bearer token".into()))
                }
            }
            "basic" => {
                let user = credentials.principal.as_deref().unwrap_or("");
                let pass = credentials.credentials.as_deref().unwrap_or("");
                if self.provider.check_basic(user, pass) {
                    Ok(())
                } else {
                    Err(BoltError::Authentication("invalid credentials".into()))
                }
            }
            "none" => Ok(()),
            other => Err(BoltError::Authentication(format!(
                "unsupported auth scheme: {other}"
            ))),
        }
    }
}
