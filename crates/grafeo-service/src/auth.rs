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

#[cfg(all(test, feature = "auth"))]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // AuthProvider::new
    // -----------------------------------------------------------------------

    #[test]
    fn new_returns_none_when_no_credentials() {
        assert!(AuthProvider::new(None, None, None).is_none());
    }

    #[test]
    fn new_returns_some_with_bearer_token() {
        assert!(AuthProvider::new(Some("tok".into()), None, None).is_some());
    }

    #[test]
    fn new_returns_some_with_basic_credentials() {
        assert!(AuthProvider::new(None, Some("user".into()), Some("pass".into())).is_some());
    }

    #[test]
    fn new_returns_some_with_user_only() {
        // User without password still creates a provider (is_enabled will be true,
        // but check_basic will always fail because password is None).
        assert!(AuthProvider::new(None, Some("user".into()), None).is_some());
    }

    // -----------------------------------------------------------------------
    // is_enabled
    // -----------------------------------------------------------------------

    #[test]
    fn is_enabled_with_bearer() {
        let p = AuthProvider::new(Some("tok".into()), None, None).unwrap();
        assert!(p.is_enabled());
    }

    #[test]
    fn is_enabled_with_basic() {
        let p = AuthProvider::new(None, Some("u".into()), Some("p".into())).unwrap();
        assert!(p.is_enabled());
    }

    // -----------------------------------------------------------------------
    // check_bearer
    // -----------------------------------------------------------------------

    #[test]
    fn check_bearer_exact_match() {
        let p = AuthProvider::new(Some("secret-token".into()), None, None).unwrap();
        assert!(p.check_bearer("secret-token"));
    }

    #[test]
    fn check_bearer_mismatch() {
        let p = AuthProvider::new(Some("secret-token".into()), None, None).unwrap();
        assert!(!p.check_bearer("wrong-token"));
    }

    #[test]
    fn check_bearer_empty_token() {
        let p = AuthProvider::new(Some("secret-token".into()), None, None).unwrap();
        assert!(!p.check_bearer(""));
    }

    #[test]
    fn check_bearer_prefix_not_enough() {
        let p = AuthProvider::new(Some("secret-token".into()), None, None).unwrap();
        assert!(!p.check_bearer("secret"));
    }

    #[test]
    fn check_bearer_case_sensitive() {
        let p = AuthProvider::new(Some("Secret".into()), None, None).unwrap();
        assert!(!p.check_bearer("secret"));
    }

    #[test]
    fn check_bearer_no_token_configured() {
        let p = AuthProvider::new(None, Some("user".into()), Some("pass".into())).unwrap();
        assert!(!p.check_bearer("anything"));
    }

    // -----------------------------------------------------------------------
    // check_basic
    // -----------------------------------------------------------------------

    #[test]
    fn check_basic_exact_match() {
        let p = AuthProvider::new(None, Some("admin".into()), Some("pass123".into())).unwrap();
        assert!(p.check_basic("admin", "pass123"));
    }

    #[test]
    fn check_basic_wrong_password() {
        let p = AuthProvider::new(None, Some("admin".into()), Some("pass123".into())).unwrap();
        assert!(!p.check_basic("admin", "wrong"));
    }

    #[test]
    fn check_basic_wrong_user() {
        let p = AuthProvider::new(None, Some("admin".into()), Some("pass123".into())).unwrap();
        assert!(!p.check_basic("other", "pass123"));
    }

    #[test]
    fn check_basic_both_wrong() {
        let p = AuthProvider::new(None, Some("admin".into()), Some("pass123".into())).unwrap();
        assert!(!p.check_basic("other", "wrong"));
    }

    #[test]
    fn check_basic_empty_credentials() {
        let p = AuthProvider::new(None, Some("admin".into()), Some("pass123".into())).unwrap();
        assert!(!p.check_basic("", ""));
    }

    #[test]
    fn check_basic_no_password_configured() {
        // User is set but password is None: check_basic always fails.
        let p = AuthProvider::new(None, Some("admin".into()), None).unwrap();
        assert!(!p.check_basic("admin", "anything"));
    }

    #[test]
    fn check_basic_no_basic_configured() {
        // Only bearer configured: check_basic always fails.
        let p = AuthProvider::new(Some("token".into()), None, None).unwrap();
        assert!(!p.check_basic("admin", "pass"));
    }

    // -----------------------------------------------------------------------
    // ct_eq (constant-time comparison)
    // -----------------------------------------------------------------------

    #[test]
    fn ct_eq_identical() {
        assert!(ct_eq(b"hello", b"hello"));
    }

    #[test]
    fn ct_eq_different_content() {
        assert!(!ct_eq(b"hello", b"world"));
    }

    #[test]
    fn ct_eq_different_length() {
        assert!(!ct_eq(b"short", b"longer"));
    }

    #[test]
    fn ct_eq_empty() {
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn ct_eq_one_empty() {
        assert!(!ct_eq(b"", b"a"));
    }
}
