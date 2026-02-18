//! Transport-agnostic authentication provider.
//!
//! Credential extraction is transport-specific (HTTP headers, gRPC metadata,
//! Bolt LOGON). This module only handles credential verification.

#[cfg(feature = "auth")]
use subtle::ConstantTimeEq;

/// Authentication provider supporting bearer tokens and HTTP Basic auth.
#[cfg(feature = "auth")]
#[derive(Clone)]
pub struct AuthProvider {
    bearer_token: Option<String>,
    basic_user: Option<String>,
    basic_password: Option<String>,
}

#[cfg(feature = "auth")]
impl AuthProvider {
    /// Creates an auth provider if any credentials are configured.
    /// Returns `None` if no authentication is set up.
    pub fn new(
        token: Option<String>,
        user: Option<String>,
        password: Option<String>,
    ) -> Option<Self> {
        if token.is_none() && user.is_none() {
            return None;
        }
        Some(Self {
            bearer_token: token,
            basic_user: user,
            basic_password: password,
        })
    }

    /// Whether any authentication method is configured.
    pub fn is_enabled(&self) -> bool {
        self.bearer_token.is_some() || self.basic_user.is_some()
    }

    /// Check a bearer token or API key.
    pub fn check_bearer(&self, token: &str) -> bool {
        self.bearer_token
            .as_ref()
            .is_some_and(|expected| ct_eq(token.as_bytes(), expected.as_bytes()))
    }

    /// Check basic auth credentials.
    pub fn check_basic(&self, user: &str, password: &str) -> bool {
        match (&self.basic_user, &self.basic_password) {
            (Some(expected_user), Some(expected_pass)) => {
                ct_eq(user.as_bytes(), expected_user.as_bytes())
                    && ct_eq(password.as_bytes(), expected_pass.as_bytes())
            }
            _ => false,
        }
    }
}

/// Constant-time comparison of two byte slices.
#[cfg(feature = "auth")]
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.ct_eq(b).into()
}
