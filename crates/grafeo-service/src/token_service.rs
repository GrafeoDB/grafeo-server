//! Token CRUD operations — create, list, revoke, get.

use sha2::{Digest, Sha256};

use crate::auth::{TokenRecord, TokenScope, role_to_str};
use crate::error::ServiceError;
use crate::token_store::TokenStore;
use crate::types;

/// Stateless token management operations.
pub struct TokenService;

impl TokenService {
    /// Create a new API token. Returns the record and the plaintext token
    /// (which is only available at creation time).
    ///
    /// `expires_in` is an optional lifetime in seconds. If set, the token
    /// will expire after that duration.
    pub fn create_token(
        store: &TokenStore,
        name: String,
        scope: types::TokenScopeRequest,
        expires_in: Option<u64>,
    ) -> Result<(TokenRecord, String), ServiceError> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(ServiceError::BadRequest(
                "token name must not be empty".to_string(),
            ));
        }
        let role = scope.to_role()?;
        let id = uuid::Uuid::new_v4().to_string();
        let plaintext = generate_token();
        let token_hash = hash_token(&plaintext);

        let expires_at = expires_in.map(|secs| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64
                + secs as i64
        });

        let record = TokenRecord {
            id,
            name,
            token_hash,
            scope: TokenScope {
                role,
                databases: scope.databases,
            },
            created_at: chrono::Utc::now().to_rfc3339(),
            expires_at,
        };

        store.insert(record.clone()).map_err(|e| {
            if e.starts_with("a token named") {
                ServiceError::Conflict(e)
            } else {
                ServiceError::Internal(format!("failed to store token: {e}"))
            }
        })?;

        tracing::info!(token_id = %record.id, token_name = %record.name, "API token created");

        Ok((record, plaintext))
    }

    /// List all tokens (records only, no plaintext).
    pub fn list_tokens(store: &TokenStore) -> Vec<types::TokenResponse> {
        store
            .list()
            .into_iter()
            .map(|r| types::TokenResponse {
                id: r.id,
                name: r.name,
                scope: types::TokenScopeRequest {
                    role: role_to_str(r.scope.role).to_string(),
                    databases: r.scope.databases,
                },
                created_at: r.created_at,
                expires_at: r.expires_at,
                token: None,
            })
            .collect()
    }

    /// Get a single token by ID.
    pub fn get_token(store: &TokenStore, id: &str) -> Result<types::TokenResponse, ServiceError> {
        let record = store
            .get(id)
            .ok_or_else(|| ServiceError::NotFound(format!("token '{id}' not found")))?;

        Ok(types::TokenResponse {
            id: record.id,
            name: record.name,
            scope: types::TokenScopeRequest {
                role: role_to_str(record.scope.role).to_string(),
                databases: record.scope.databases,
            },
            created_at: record.created_at,
            expires_at: record.expires_at,
            token: None,
        })
    }

    /// Revoke (delete) a token by ID.
    pub fn delete_token(store: &TokenStore, id: &str) -> Result<(), ServiceError> {
        let removed = store
            .remove(id)
            .map_err(|e| ServiceError::Internal(format!("failed to remove token: {e}")))?;

        if !removed {
            return Err(ServiceError::NotFound(format!("token '{id}' not found")));
        }

        tracing::info!(token_id = %id, "API token revoked");
        Ok(())
    }
}

/// Generate a cryptographically random token (32 bytes, hex-encoded = 64 chars).
fn generate_token() -> String {
    use rand::Rng;
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    hex::encode(bytes)
}

/// SHA-256 hash a plaintext token, return hex string.
pub fn hash_token(plaintext: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(plaintext.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TokenScopeRequest;

    fn make_store() -> TokenStore {
        let dir = tempfile::tempdir().unwrap();
        // Leak the tempdir so it stays alive for the test
        let path = dir.into_path().join("tokens.json");
        TokenStore::load(path).unwrap()
    }

    fn default_scope() -> TokenScopeRequest {
        TokenScopeRequest::default()
    }

    #[test]
    fn create_token_returns_plaintext_and_record() {
        let store = make_store();
        let (record, plaintext) =
            TokenService::create_token(&store, "test".to_string(), default_scope(), None).unwrap();
        assert!(!plaintext.is_empty());
        assert_ne!(plaintext, record.token_hash);
        assert_eq!(record.name, "test");
    }

    #[test]
    fn create_token_hash_matches_sha256() {
        let store = make_store();
        let (record, plaintext) =
            TokenService::create_token(&store, "test".to_string(), default_scope(), None).unwrap();
        assert_eq!(hash_token(&plaintext), record.token_hash);
    }

    #[test]
    fn create_token_generates_unique_ids() {
        let store = make_store();
        let (r1, _) =
            TokenService::create_token(&store, "a".into(), default_scope(), None).unwrap();
        let (r2, _) =
            TokenService::create_token(&store, "b".into(), default_scope(), None).unwrap();
        assert_ne!(r1.id, r2.id);
    }

    #[test]
    fn create_token_generates_unique_tokens() {
        let store = make_store();
        let (_, t1) =
            TokenService::create_token(&store, "a".into(), default_scope(), None).unwrap();
        let (_, t2) =
            TokenService::create_token(&store, "b".into(), default_scope(), None).unwrap();
        assert_ne!(t1, t2);
    }

    #[test]
    fn create_token_persists_to_store() {
        let store = make_store();
        let (record, plaintext) =
            TokenService::create_token(&store, "test".into(), default_scope(), None).unwrap();
        let found = store.find_by_hash(&hash_token(&plaintext));
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, record.id);
    }

    #[test]
    fn list_tokens_empty() {
        let store = make_store();
        assert!(TokenService::list_tokens(&store).is_empty());
    }

    #[test]
    fn list_tokens_returns_all() {
        let store = make_store();
        TokenService::create_token(&store, "a".into(), default_scope(), None).unwrap();
        TokenService::create_token(&store, "b".into(), default_scope(), None).unwrap();
        TokenService::create_token(&store, "c".into(), default_scope(), None).unwrap();
        assert_eq!(TokenService::list_tokens(&store).len(), 3);
    }

    #[test]
    fn delete_token_removes_from_store() {
        let store = make_store();
        let (record, _) =
            TokenService::create_token(&store, "x".into(), default_scope(), None).unwrap();
        TokenService::delete_token(&store, &record.id).unwrap();
        assert!(store.get(&record.id).is_none());
    }

    #[test]
    fn delete_token_not_found() {
        let store = make_store();
        let err = TokenService::delete_token(&store, "nonexistent").unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[test]
    fn get_token_found() {
        let store = make_store();
        let (record, _) =
            TokenService::create_token(&store, "x".into(), default_scope(), None).unwrap();
        let resp = TokenService::get_token(&store, &record.id).unwrap();
        assert_eq!(resp.name, "x");
        assert!(resp.token.is_none()); // no plaintext in get
    }

    #[test]
    fn get_token_not_found() {
        let store = make_store();
        let err = TokenService::get_token(&store, "nonexistent").unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[test]
    fn create_token_rejects_empty_name() {
        let store = make_store();
        let err =
            TokenService::create_token(&store, "".to_string(), default_scope(), None).unwrap_err();
        assert!(matches!(err, ServiceError::BadRequest(_)));
    }

    #[test]
    fn create_token_rejects_whitespace_only_name() {
        let store = make_store();
        let err = TokenService::create_token(&store, "   ".to_string(), default_scope(), None)
            .unwrap_err();
        assert!(matches!(err, ServiceError::BadRequest(_)));
    }

    #[test]
    fn create_token_rejects_duplicate_name() {
        let store = make_store();
        TokenService::create_token(&store, "dup".to_string(), default_scope(), None).unwrap();
        let err = TokenService::create_token(&store, "dup".to_string(), default_scope(), None)
            .unwrap_err();
        assert!(matches!(err, ServiceError::Conflict(_)));
    }

    #[test]
    fn create_token_scope_preserved() {
        let store = make_store();
        let scope = TokenScopeRequest {
            role: "read-only".to_string(),
            databases: vec!["db1".to_string(), "db2".to_string()],
        };
        let (record, _) = TokenService::create_token(&store, "scoped".into(), scope, None).unwrap();
        assert_eq!(record.scope.role, grafeo_engine::auth::Role::ReadOnly);
        assert_eq!(record.scope.databases, vec!["db1", "db2"]);
    }
}
