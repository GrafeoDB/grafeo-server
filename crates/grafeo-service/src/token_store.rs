//! File-based token store with atomic writes.
//!
//! Tokens are stored as a JSON array in `{data_dir}/tokens.json`.
//! Reads go through a `RwLock` for concurrent access. Writes serialize
//! to a temp file and rename atomically.

use std::path::PathBuf;

use parking_lot::RwLock;

use crate::auth::TokenRecord;

/// Persistent token storage backed by a JSON file.
pub struct TokenStore {
    path: PathBuf,
    tokens: RwLock<Vec<TokenRecord>>,
}

impl TokenStore {
    /// Load tokens from disk. Creates an empty file if it doesn't exist.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();

        let tokens = if path.exists() {
            let contents = std::fs::read_to_string(&path)
                .map_err(|e| format!("failed to read token store: {e}"))?;
            if contents.trim().is_empty() {
                vec![]
            } else {
                serde_json::from_str(&contents)
                    .map_err(|e| format!("failed to parse token store: {e}"))?
            }
        } else {
            // Create parent dirs and empty file
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("failed to create token store directory: {e}"))?;
            }
            std::fs::write(&path, "[]")
                .map_err(|e| format!("failed to create token store: {e}"))?;
            vec![]
        };

        tracing::info!(path = %path.display(), count = tokens.len(), "Token store loaded");

        Ok(Self {
            path,
            tokens: RwLock::new(tokens),
        })
    }

    /// Serialize `tokens` to JSON and write to disk atomically (write tmp, rename).
    ///
    /// Caller must hold the write lock to prevent concurrent saves from
    /// clobbering each other's temp file.
    fn save_locked(tokens: &[TokenRecord], path: &std::path::Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(tokens)
            .map_err(|e| format!("failed to serialize tokens: {e}"))?;

        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, &json)
            .map_err(|e| format!("failed to write token store tmp: {e}"))?;
        std::fs::rename(&tmp_path, path)
            .map_err(|e| format!("failed to rename token store: {e}"))?;

        Ok(())
    }

    /// Insert a token record and persist to disk.
    ///
    /// Returns `Err` if a token with the same name already exists (checked
    /// under the write lock so the guarantee is atomic).
    pub fn insert(&self, record: TokenRecord) -> Result<(), String> {
        let mut tokens = self.tokens.write();
        if tokens.iter().any(|t| t.name == record.name) {
            return Err(format!("a token named '{}' already exists", record.name));
        }
        tokens.push(record);
        Self::save_locked(&tokens, &self.path)
    }

    /// Remove a token by ID. Returns true if found and removed.
    pub fn remove(&self, id: &str) -> Result<bool, String> {
        let mut tokens = self.tokens.write();
        let before = tokens.len();
        tokens.retain(|t| t.id != id);
        let removed = tokens.len() < before;
        if removed {
            Self::save_locked(&tokens, &self.path)?;
        }
        Ok(removed)
    }

    /// List all token records.
    pub fn list(&self) -> Vec<TokenRecord> {
        self.tokens.read().clone()
    }

    /// Find a token record by its SHA-256 hash.
    pub fn find_by_hash(&self, hash: &str) -> Option<TokenRecord> {
        self.tokens
            .read()
            .iter()
            .find(|t| t.token_hash == hash)
            .cloned()
    }

    /// Get a token record by ID.
    pub fn get(&self, id: &str) -> Option<TokenRecord> {
        self.tokens.read().iter().find(|t| t.id == id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::TokenScope;

    fn make_record(id: &str, hash: &str) -> TokenRecord {
        TokenRecord {
            id: id.to_string(),
            name: format!("token-{id}"),
            token_hash: hash.to_string(),
            scope: TokenScope::default(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn load_creates_file_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        assert!(!path.exists());
        let store = TokenStore::load(&path).unwrap();
        assert!(path.exists());
        assert!(store.list().is_empty());
    }

    #[test]
    fn load_reads_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let record = make_record("abc", "hash123");
        std::fs::write(&path, serde_json::to_string(&vec![&record]).unwrap()).unwrap();

        let store = TokenStore::load(&path).unwrap();
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.list()[0].id, "abc");
    }

    #[test]
    fn load_handles_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        std::fs::write(&path, "").unwrap();
        let store = TokenStore::load(&path).unwrap();
        assert!(store.list().is_empty());
    }

    #[test]
    fn load_handles_malformed_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        std::fs::write(&path, "not json").unwrap();
        assert!(TokenStore::load(&path).is_err());
    }

    #[test]
    fn insert_and_find_by_hash() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::load(dir.path().join("tokens.json")).unwrap();
        store.insert(make_record("id1", "hash1")).unwrap();

        let found = store.find_by_hash("hash1").unwrap();
        assert_eq!(found.id, "id1");
    }

    #[test]
    fn find_by_hash_returns_none_for_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::load(dir.path().join("tokens.json")).unwrap();
        assert!(store.find_by_hash("nonexistent").is_none());
    }

    #[test]
    fn remove_existing_token() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::load(dir.path().join("tokens.json")).unwrap();
        store.insert(make_record("id1", "hash1")).unwrap();
        assert!(store.remove("id1").unwrap());
        assert!(store.find_by_hash("hash1").is_none());
    }

    #[test]
    fn remove_nonexistent_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::load(dir.path().join("tokens.json")).unwrap();
        assert!(!store.remove("nonexistent").unwrap());
    }

    #[test]
    fn list_returns_all_records() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::load(dir.path().join("tokens.json")).unwrap();
        store.insert(make_record("a", "h1")).unwrap();
        store.insert(make_record("b", "h2")).unwrap();
        store.insert(make_record("c", "h3")).unwrap();
        assert_eq!(store.list().len(), 3);
    }

    #[test]
    fn save_persists_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let store = TokenStore::load(&path).unwrap();
        store.insert(make_record("id1", "hash1")).unwrap();
        drop(store);

        // Reload from disk
        let store2 = TokenStore::load(&path).unwrap();
        assert_eq!(store2.list().len(), 1);
        assert_eq!(store2.list()[0].id, "id1");
    }

    #[test]
    fn save_no_tmp_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let store = TokenStore::load(&path).unwrap();
        store.insert(make_record("id1", "hash1")).unwrap();
        assert!(!dir.path().join("tokens.json.tmp").exists());
    }

    #[test]
    fn get_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::load(dir.path().join("tokens.json")).unwrap();
        store.insert(make_record("abc", "hash1")).unwrap();
        let found = store.get("abc").unwrap();
        assert_eq!(found.token_hash, "hash1");
        assert!(store.get("nonexistent").is_none());
    }
}
