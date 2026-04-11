//! Transport-agnostic authentication provider.
//!
//! Credential extraction is transport-specific (HTTP headers, gRPC metadata,
//! Bolt LOGON). This module only handles credential verification.

use serde::{Deserialize, Serialize};
#[cfg(feature = "auth")]
use subtle::ConstantTimeEq;

pub use grafeo_engine::auth::{Identity, Role};

/// Map a [`Role`] to the wire-format string used in JSON storage and API responses.
pub fn role_to_str(role: Role) -> &'static str {
    match role {
        Role::Admin => "admin",
        Role::ReadWrite => "read-write",
        Role::ReadOnly => "read-only",
    }
}

/// Cap an [`Identity`] to [`Role::ReadOnly`] when the server is in read-only mode.
///
/// Returns the identity unchanged when `read_only` is `false`.
pub fn cap_identity_read_only(identity: Identity, read_only: bool) -> Identity {
    if read_only {
        Identity::new(identity.user_id(), [Role::ReadOnly])
    } else {
        identity
    }
}

/// Spawns a background task that periodically removes stale entries from a
/// pending-auth map. Returns the join handle so the caller can abort it on
/// shutdown.
///
/// Used by GWP and BoltR transports to clean up nonces from clients that
/// authenticated but never completed session creation.
pub fn spawn_pending_auth_reaper<V: Send + Sync + 'static>(
    map: std::sync::Arc<dashmap::DashMap<String, (V, std::time::Instant)>>,
    ttl: std::time::Duration,
    interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            let now = std::time::Instant::now();
            map.retain(|_, (_, created)| now.duration_since(*created) < ttl);
        }
    })
}

/// Parse a wire-format string into a [`Role`].
pub fn str_to_role(s: &str) -> Result<Role, String> {
    match s {
        "admin" => Ok(Role::Admin),
        "read-write" => Ok(Role::ReadWrite),
        "read-only" => Ok(Role::ReadOnly),
        _ => Err(format!("unknown role: {s}")),
    }
}

mod role_serde {
    use super::*;

    #[allow(clippy::trivially_copy_pass_by_ref)] // serde requires &T
    pub fn serialize<S: serde::Serializer>(role: &Role, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(role_to_str(*role))
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Role, D::Error> {
        let s = String::deserialize(d)?;
        str_to_role(&s).map_err(serde::de::Error::custom)
    }
}

/// Permission scope for a token: role + database restrictions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TokenScope {
    /// Permission level: "admin", "read-write", or "read-only".
    #[serde(with = "role_serde")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, example = "admin"))]
    pub role: Role,
    /// Databases this token can access. Empty vec = all databases.
    #[serde(default)]
    pub databases: Vec<String>,
}

impl Default for TokenScope {
    fn default() -> Self {
        Self {
            role: Role::Admin,
            databases: vec![],
        }
    }
}

/// On-disk record (hashed token, never the plaintext).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRecord {
    pub id: String,
    pub name: String,
    pub token_hash: String,
    pub scope: TokenScope,
    pub created_at: String,
}

/// In-flight identity attached to a request after authentication.
#[derive(Debug, Clone)]
pub struct TokenInfo {
    pub id: String,
    pub name: String,
    pub scope: TokenScope,
}

impl TokenInfo {
    /// Construct an engine [`Identity`] from this token's metadata.
    pub fn identity(&self) -> Identity {
        Identity::new(&self.name, [self.scope.role])
    }
}

/// Authentication provider supporting bearer tokens, token store, and HTTP Basic auth.
///
/// `check_bearer` checks the legacy single token first (from `--auth-token`),
/// then the token store. Returns `Option<TokenInfo>` with the token's scope.
/// Basic auth always returns admin scope.
#[cfg(feature = "auth")]
#[derive(Clone)]
pub struct AuthProvider {
    /// Legacy single bearer token (from --auth-token CLI flag).
    bearer_token: Option<String>,
    basic_user: Option<String>,
    basic_password: Option<String>,
    /// Token store for managed API keys (from token CRUD API).
    token_store: Option<std::sync::Arc<crate::token_store::TokenStore>>,
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
            token_store: None,
        })
    }

    /// Creates a provider with a token store for managed API keys.
    pub fn with_token_store(
        token: Option<String>,
        user: Option<String>,
        password: Option<String>,
        store: std::sync::Arc<crate::token_store::TokenStore>,
    ) -> Option<Self> {
        // If we have a store, we always enable auth (even without a legacy token,
        // managed tokens will authenticate via the store).
        Some(Self {
            bearer_token: token,
            basic_user: user,
            basic_password: password,
            token_store: Some(store),
        })
    }

    /// Whether any authentication method is configured.
    pub fn is_enabled(&self) -> bool {
        self.bearer_token.is_some() || self.basic_user.is_some() || self.token_store.is_some()
    }

    /// Returns a reference to the token store, if configured.
    pub fn token_store(&self) -> Option<&crate::token_store::TokenStore> {
        self.token_store.as_deref()
    }

    /// Check a bearer token or API key. Returns token identity on success.
    ///
    /// Checks the legacy single token first (admin scope), then the token
    /// store. Returns `None` if no match.
    pub fn check_bearer(&self, token: &str) -> Option<TokenInfo> {
        // Check legacy single token (constant-time)
        let legacy_match = self
            .bearer_token
            .as_ref()
            .is_some_and(|expected| ct_eq(token.as_bytes(), expected.as_bytes()));

        if legacy_match {
            return Some(TokenInfo {
                id: "_root".to_string(),
                name: "root".to_string(),
                scope: TokenScope::default(), // admin, all databases
            });
        }

        // Check token store
        if let Some(store) = &self.token_store {
            let hash = crate::token_service::hash_token(token);
            if let Some(record) = store.find_by_hash(&hash) {
                return Some(TokenInfo {
                    id: record.id,
                    name: record.name,
                    scope: record.scope,
                });
            }
        }

        None
    }

    /// Check basic auth credentials. Returns true on match.
    /// Basic auth always grants admin scope (no per-database scoping).
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
///
/// The `subtle` crate's `ct_eq` for `[u8]` returns `false` when lengths
/// differ, so no explicit length guard is needed. Avoiding a short-circuit
/// length check prevents leaking the secret's length via timing.
#[cfg(feature = "auth")]
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

#[cfg(all(test, feature = "auth"))]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Role serde round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn role_serde_roundtrip() {
        let scope = TokenScope {
            role: Role::ReadWrite,
            databases: vec!["mydb".to_string()],
        };
        let json = serde_json::to_string(&scope).unwrap();
        assert!(json.contains("\"read-write\""));
        let parsed: TokenScope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.role, Role::ReadWrite);
    }

    #[test]
    fn role_serde_all_variants() {
        for (role, expected) in [
            (Role::Admin, "admin"),
            (Role::ReadWrite, "read-write"),
            (Role::ReadOnly, "read-only"),
        ] {
            assert_eq!(role_to_str(role), expected);
            assert_eq!(str_to_role(expected).unwrap(), role);
        }
    }

    #[test]
    fn str_to_role_unknown() {
        assert!(str_to_role("superuser").is_err());
    }

    // -----------------------------------------------------------------------
    // TokenInfo::identity
    // -----------------------------------------------------------------------

    #[test]
    fn token_info_identity() {
        let info = TokenInfo {
            id: "tok-1".to_string(),
            name: "my-service".to_string(),
            scope: TokenScope {
                role: Role::ReadOnly,
                databases: vec![],
            },
        };
        let id = info.identity();
        assert_eq!(id.user_id(), "my-service");
        assert!(id.can_read());
        assert!(!id.can_write());
    }

    // -----------------------------------------------------------------------
    // cap_identity_read_only
    // -----------------------------------------------------------------------

    #[test]
    fn cap_identity_passthrough_when_not_read_only() {
        let id = Identity::new("alice", [Role::Admin]);
        let capped = cap_identity_read_only(id, false);
        assert!(capped.can_admin());
        assert_eq!(capped.user_id(), "alice");
    }

    #[test]
    fn cap_identity_capped_when_read_only() {
        let id = Identity::new("alice", [Role::Admin]);
        let capped = cap_identity_read_only(id, true);
        assert!(capped.can_read());
        assert!(!capped.can_write());
        assert_eq!(capped.user_id(), "alice");
    }

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
        assert!(p.check_bearer("secret-token").is_some());
    }

    #[test]
    fn check_bearer_returns_admin_scope() {
        let p = AuthProvider::new(Some("secret-token".into()), None, None).unwrap();
        let info = p.check_bearer("secret-token").unwrap();
        assert_eq!(info.scope.role, Role::Admin);
        assert!(info.scope.databases.is_empty());
    }

    #[test]
    fn check_bearer_mismatch() {
        let p = AuthProvider::new(Some("secret-token".into()), None, None).unwrap();
        assert!(p.check_bearer("wrong-token").is_none());
    }

    #[test]
    fn check_bearer_empty_token() {
        let p = AuthProvider::new(Some("secret-token".into()), None, None).unwrap();
        assert!(p.check_bearer("").is_none());
    }

    #[test]
    fn check_bearer_prefix_not_enough() {
        let p = AuthProvider::new(Some("secret-token".into()), None, None).unwrap();
        assert!(p.check_bearer("secret").is_none());
    }

    #[test]
    fn check_bearer_case_sensitive() {
        let p = AuthProvider::new(Some("Secret".into()), None, None).unwrap();
        assert!(p.check_bearer("secret").is_none());
    }

    #[test]
    fn check_bearer_no_token_configured() {
        let p = AuthProvider::new(None, Some("user".into()), Some("pass".into())).unwrap();
        assert!(p.check_bearer("anything").is_none());
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
        let p = AuthProvider::new(None, Some("admin".into()), None).unwrap();
        assert!(!p.check_basic("admin", "anything"));
    }

    #[test]
    fn check_basic_no_basic_configured() {
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

    // -----------------------------------------------------------------------
    // role_serde: deserialization of invalid role
    // -----------------------------------------------------------------------

    #[test]
    fn role_serde_invalid_role_string() {
        let json = r#"{"role":"superuser","databases":[]}"#;
        let result: Result<TokenScope, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // TokenScope default
    // -----------------------------------------------------------------------

    #[test]
    fn token_scope_default_is_admin_all_dbs() {
        let scope = TokenScope::default();
        assert_eq!(scope.role, Role::Admin);
        assert!(scope.databases.is_empty());
    }

    // -----------------------------------------------------------------------
    // TokenInfo::identity for different roles
    // -----------------------------------------------------------------------

    #[test]
    fn token_info_identity_admin() {
        let info = TokenInfo {
            id: "tok-2".to_string(),
            name: "admin-svc".to_string(),
            scope: TokenScope {
                role: Role::Admin,
                databases: vec![],
            },
        };
        let id = info.identity();
        assert_eq!(id.user_id(), "admin-svc");
        assert!(id.can_admin());
        assert!(id.can_write());
        assert!(id.can_read());
    }

    #[test]
    fn token_info_identity_read_write() {
        let info = TokenInfo {
            id: "tok-3".to_string(),
            name: "rw-svc".to_string(),
            scope: TokenScope {
                role: Role::ReadWrite,
                databases: vec![],
            },
        };
        let id = info.identity();
        assert!(id.can_write());
        assert!(id.can_read());
        assert!(!id.can_admin());
    }

    // -----------------------------------------------------------------------
    // AuthProvider::with_token_store
    // -----------------------------------------------------------------------

    #[test]
    fn with_token_store_creates_provider_even_without_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::token_store::TokenStore::load(dir.path().join("tokens.json")).unwrap();
        let store_arc = std::sync::Arc::new(store);
        let provider = AuthProvider::with_token_store(None, None, None, store_arc);
        assert!(provider.is_some());
        let p = provider.unwrap();
        assert!(p.is_enabled());
        assert!(p.token_store().is_some());
    }

    #[test]
    fn with_token_store_and_legacy_token() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::token_store::TokenStore::load(dir.path().join("tokens.json")).unwrap();
        let store_arc = std::sync::Arc::new(store);
        let provider =
            AuthProvider::with_token_store(Some("tok".into()), None, None, store_arc).unwrap();
        assert!(provider.is_enabled());
        // Legacy token should still work
        assert!(provider.check_bearer("tok").is_some());
    }

    // -----------------------------------------------------------------------
    // check_bearer: store lookup
    // -----------------------------------------------------------------------

    #[test]
    fn check_bearer_looks_up_token_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::token_store::TokenStore::load(dir.path().join("tokens.json")).unwrap();

        // Insert a token record into the store.
        let plaintext = "managed-api-key-123";
        let hash = crate::token_service::hash_token(plaintext);
        store
            .insert(TokenRecord {
                id: "tok-managed".to_string(),
                name: "my-key".to_string(),
                token_hash: hash,
                scope: TokenScope {
                    role: Role::ReadOnly,
                    databases: vec!["mydb".to_string()],
                },
                created_at: "2024-01-01T00:00:00Z".to_string(),
            })
            .unwrap();

        let store_arc = std::sync::Arc::new(store);
        let provider =
            AuthProvider::with_token_store(Some("legacy-tok".into()), None, None, store_arc)
                .unwrap();

        // Legacy token returns admin scope.
        let legacy = provider.check_bearer("legacy-tok").unwrap();
        assert_eq!(legacy.scope.role, Role::Admin);

        // Managed token returns its own scope.
        let managed = provider.check_bearer(plaintext).unwrap();
        assert_eq!(managed.id, "tok-managed");
        assert_eq!(managed.name, "my-key");
        assert_eq!(managed.scope.role, Role::ReadOnly);
        assert_eq!(managed.scope.databases, vec!["mydb".to_string()]);

        // Unknown token returns None.
        assert!(provider.check_bearer("unknown").is_none());
    }

    #[test]
    fn check_bearer_store_only_no_legacy() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::token_store::TokenStore::load(dir.path().join("tokens.json")).unwrap();

        let plaintext = "store-only-key";
        let hash = crate::token_service::hash_token(plaintext);
        store
            .insert(TokenRecord {
                id: "tok-store".to_string(),
                name: "store-key".to_string(),
                token_hash: hash,
                scope: TokenScope::default(),
                created_at: "2024-01-01T00:00:00Z".to_string(),
            })
            .unwrap();

        let store_arc = std::sync::Arc::new(store);
        let provider = AuthProvider::with_token_store(None, None, None, store_arc).unwrap();

        // No legacy token, so only store lookup works.
        let info = provider.check_bearer(plaintext).unwrap();
        assert_eq!(info.id, "tok-store");

        // Random token returns None.
        assert!(provider.check_bearer("random").is_none());
    }

    // -----------------------------------------------------------------------
    // token_store accessor
    // -----------------------------------------------------------------------

    #[test]
    fn token_store_returns_none_without_store() {
        let provider = AuthProvider::new(Some("tok".into()), None, None).unwrap();
        assert!(provider.token_store().is_none());
    }

    // -----------------------------------------------------------------------
    // with_token_store: all credential combinations
    // -----------------------------------------------------------------------

    #[test]
    fn with_token_store_all_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::token_store::TokenStore::load(dir.path().join("tokens.json")).unwrap();
        let store_arc = std::sync::Arc::new(store);
        let provider = AuthProvider::with_token_store(
            Some("tok".into()),
            Some("user".into()),
            Some("pass".into()),
            store_arc,
        )
        .unwrap();
        assert!(provider.is_enabled());
        assert!(provider.token_store().is_some());
        // Legacy bearer still works
        assert!(provider.check_bearer("tok").is_some());
        // Basic auth still works
        assert!(provider.check_basic("user", "pass"));
    }

    #[test]
    fn with_token_store_basic_only_plus_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::token_store::TokenStore::load(dir.path().join("tokens.json")).unwrap();
        let store_arc = std::sync::Arc::new(store);
        let provider =
            AuthProvider::with_token_store(None, Some("u".into()), Some("p".into()), store_arc)
                .unwrap();
        assert!(provider.is_enabled());
        assert!(provider.check_basic("u", "p"));
        assert!(provider.check_bearer("anything").is_none());
    }

    // -----------------------------------------------------------------------
    // check_bearer: legacy miss, store hit
    // -----------------------------------------------------------------------

    #[test]
    fn check_bearer_legacy_miss_store_hit() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::token_store::TokenStore::load(dir.path().join("tokens.json")).unwrap();

        let plaintext = "managed-key-xyz";
        let hash = crate::token_service::hash_token(plaintext);
        store
            .insert(TokenRecord {
                id: "tok-m".to_string(),
                name: "m-key".to_string(),
                token_hash: hash,
                scope: TokenScope {
                    role: Role::ReadWrite,
                    databases: vec!["db1".to_string()],
                },
                created_at: "2024-06-01T00:00:00Z".to_string(),
            })
            .unwrap();

        let store_arc = std::sync::Arc::new(store);
        // Legacy token is "legacy-secret", but we query with the managed key
        let provider =
            AuthProvider::with_token_store(Some("legacy-secret".into()), None, None, store_arc)
                .unwrap();

        // The managed key does NOT match legacy, but IS found in the store
        let info = provider.check_bearer(plaintext).unwrap();
        assert_eq!(info.id, "tok-m");
        assert_eq!(info.scope.role, Role::ReadWrite);
        assert_eq!(info.scope.databases, vec!["db1".to_string()]);
    }

    #[test]
    fn check_bearer_no_legacy_no_store_match() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::token_store::TokenStore::load(dir.path().join("tokens.json")).unwrap();
        let store_arc = std::sync::Arc::new(store);
        let provider = AuthProvider::with_token_store(None, None, None, store_arc).unwrap();
        // Empty store, no legacy token: nothing matches
        assert!(provider.check_bearer("anything").is_none());
    }

    // -----------------------------------------------------------------------
    // check_basic: additional edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn check_basic_case_sensitive() {
        let p = AuthProvider::new(None, Some("Admin".into()), Some("Pass".into())).unwrap();
        assert!(!p.check_basic("admin", "Pass"));
        assert!(!p.check_basic("Admin", "pass"));
        assert!(p.check_basic("Admin", "Pass"));
    }

    #[test]
    fn check_basic_user_only_no_password_rejects_all() {
        // When only a username is configured (no password), check_basic
        // must reject every attempt because the (Some, None) arm falls
        // through to the catch-all `_ => false`.
        let p = AuthProvider::new(None, Some("admin".into()), None).unwrap();
        assert!(!p.check_basic("admin", ""));
        assert!(!p.check_basic("admin", "guess"));
        assert!(!p.check_basic("other", ""));
    }

    #[test]
    fn check_basic_token_only_provider_rejects() {
        // Provider with only a bearer token has no basic credentials at all
        let p = AuthProvider::new(Some("tok".into()), None, None).unwrap();
        assert!(!p.check_basic("", ""));
        assert!(!p.check_basic("any", "any"));
    }

    // -----------------------------------------------------------------------
    // str_to_role: error message content
    // -----------------------------------------------------------------------

    #[test]
    fn str_to_role_error_contains_input() {
        let err = str_to_role("bogus").unwrap_err();
        assert!(
            err.contains("bogus"),
            "error should mention the invalid input"
        );
    }

    // -----------------------------------------------------------------------
    // spawn_pending_auth_reaper: cleanup expired entries
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn pending_auth_reaper_removes_expired_entries() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let map: Arc<dashmap::DashMap<String, (String, Instant)>> =
            Arc::new(dashmap::DashMap::new());

        // Insert one fresh entry and one already-expired entry
        map.insert("fresh".to_string(), ("val".to_string(), Instant::now()));
        map.insert(
            "stale".to_string(),
            ("val".to_string(), Instant::now() - Duration::from_secs(10)),
        );

        assert_eq!(map.len(), 2);

        let ttl = Duration::from_secs(5);
        let interval = Duration::from_millis(50);
        let handle = spawn_pending_auth_reaper(map.clone(), ttl, interval);

        // Let the reaper tick at least once
        tokio::time::sleep(Duration::from_millis(120)).await;

        // The stale entry should be gone, the fresh one should remain
        assert!(map.contains_key("fresh"), "fresh entry should survive");
        assert!(!map.contains_key("stale"), "stale entry should be reaped");

        handle.abort();
    }

    #[tokio::test]
    async fn pending_auth_reaper_leaves_fresh_entries() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let map: Arc<dashmap::DashMap<String, (u32, Instant)>> = Arc::new(dashmap::DashMap::new());

        // All entries are fresh
        map.insert("a".to_string(), (1, Instant::now()));
        map.insert("b".to_string(), (2, Instant::now()));

        let ttl = Duration::from_secs(60);
        let interval = Duration::from_millis(50);
        let handle = spawn_pending_auth_reaper(map.clone(), ttl, interval);

        tokio::time::sleep(Duration::from_millis(120)).await;

        assert_eq!(map.len(), 2, "all fresh entries should survive");
        handle.abort();
    }

    #[tokio::test]
    async fn pending_auth_reaper_removes_all_when_all_expired() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let map: Arc<dashmap::DashMap<String, ((), Instant)>> = Arc::new(dashmap::DashMap::new());

        let old = Instant::now() - Duration::from_secs(100);
        map.insert("x".to_string(), ((), old));
        map.insert("y".to_string(), ((), old));

        let ttl = Duration::from_secs(5);
        let interval = Duration::from_millis(50);
        let handle = spawn_pending_auth_reaper(map.clone(), ttl, interval);

        tokio::time::sleep(Duration::from_millis(120)).await;

        assert!(map.is_empty(), "all expired entries should be reaped");
        handle.abort();
    }
}
