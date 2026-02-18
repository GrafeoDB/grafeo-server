//! GWP authentication adapter.
//!
//! Bridges `grafeo_service::auth::AuthProvider` to `gwp::server::AuthValidator`,
//! mapping GWP proto credentials to the transport-agnostic auth checks.

use grafeo_service::auth::AuthProvider;
use gwp::error::GqlError;
use gwp::proto;
use gwp::server::AuthValidator;

/// Validates GWP handshake credentials using the shared [`AuthProvider`].
pub(crate) struct GwpAuthValidator {
    provider: AuthProvider,
}

impl GwpAuthValidator {
    pub fn new(provider: AuthProvider) -> Self {
        Self { provider }
    }
}

#[tonic::async_trait]
impl AuthValidator for GwpAuthValidator {
    async fn validate(&self, credentials: &proto::AuthCredentials) -> Result<(), GqlError> {
        use proto::auth_credentials::Method;

        match &credentials.method {
            Some(Method::BearerToken(token)) => {
                if self.provider.check_bearer(token) {
                    Ok(())
                } else {
                    Err(GqlError::Protocol("invalid bearer token".to_owned()))
                }
            }
            Some(Method::Basic(basic)) => {
                if self.provider.check_basic(&basic.username, &basic.password) {
                    Ok(())
                } else {
                    Err(GqlError::Protocol("invalid credentials".to_owned()))
                }
            }
            None => Err(GqlError::Protocol("credentials required".to_owned())),
        }
    }
}
