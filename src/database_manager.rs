//! Multi-database registry.
//!
//! Each named database is an independent `GrafeoDB` instance with its own
//! session manager. The `"default"` database always exists and cannot be
//! deleted.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use grafeo_engine::GrafeoDB;
use serde::Serialize;
use utoipa::ToSchema;

use crate::error::ApiError;
use crate::sessions::SessionManager;

/// Name validation: starts with letter, then alphanumeric/underscore/hyphen, max 64 chars.
fn is_valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A single database instance with its own session manager.
pub struct DatabaseEntry {
    pub db: GrafeoDB,
    pub sessions: SessionManager,
}

/// Thread-safe registry of named database instances.
pub struct DatabaseManager {
    databases: DashMap<String, Arc<DatabaseEntry>>,
    /// Maps session IDs to database names for transaction routing.
    session_db_map: DashMap<String, String>,
    /// If `Some`, databases are persisted under `{data_dir}/{name}/grafeo.db`.
    data_dir: Option<PathBuf>,
}

/// Summary info returned by the list endpoint.
#[derive(Serialize, ToSchema)]
pub struct DatabaseSummary {
    /// Database name.
    pub name: String,
    /// Number of nodes.
    pub node_count: usize,
    /// Number of edges.
    pub edge_count: usize,
    /// Whether the database uses persistent storage.
    pub persistent: bool,
}

impl DatabaseManager {
    /// Creates a new manager. In persistent mode, scans `data_dir` for existing
    /// database subdirectories and opens each one. Ensures the `"default"` database
    /// always exists. Performs migration from the old single-file layout if needed.
    pub fn new(data_dir: Option<&str>) -> Self {
        let mgr = Self {
            databases: DashMap::new(),
            session_db_map: DashMap::new(),
            data_dir: data_dir.map(PathBuf::from),
        };

        if let Some(ref dir) = mgr.data_dir {
            std::fs::create_dir_all(dir).expect("failed to create data directory");

            // Migration: old layout had `{data_dir}/grafeo.db` directly.
            // Move it to `{data_dir}/default/grafeo.db`.
            let old_path = dir.join("grafeo.db");
            if old_path.exists() {
                let new_dir = dir.join("default");
                std::fs::create_dir_all(&new_dir).expect("failed to create default db directory");
                let new_path = new_dir.join("grafeo.db");
                if !new_path.exists() {
                    tracing::info!("Migrating old database layout to default/grafeo.db");
                    std::fs::rename(&old_path, &new_path).expect("failed to migrate database file");
                    // Also move WAL file if present
                    let old_wal = dir.join("grafeo.db.wal");
                    if old_wal.exists() {
                        let new_wal = new_dir.join("grafeo.db.wal");
                        let _ = std::fs::rename(&old_wal, &new_wal);
                    }
                }
            }

            // Scan subdirectories for existing databases
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let db_file = path.join("grafeo.db");
                        if db_file.exists() {
                            let name = entry.file_name().to_string_lossy().to_string();
                            tracing::info!(name = %name, "Opening database");
                            match GrafeoDB::open(db_file.to_str().unwrap()) {
                                Ok(db) => {
                                    mgr.databases.insert(
                                        name,
                                        Arc::new(DatabaseEntry {
                                            db,
                                            sessions: SessionManager::new(),
                                        }),
                                    );
                                }
                                Err(e) => {
                                    tracing::error!(name = %name, error = %e, "Failed to open database, skipping");
                                }
                            }
                        }
                    }
                }
            }

            // Ensure "default" exists
            if !mgr.databases.contains_key("default") {
                let default_dir = dir.join("default");
                std::fs::create_dir_all(&default_dir)
                    .expect("failed to create default db directory");
                let db_path = default_dir.join("grafeo.db");
                tracing::info!("Creating default persistent database");
                let db =
                    GrafeoDB::open(db_path.to_str().unwrap()).expect("failed to create default db");
                mgr.databases.insert(
                    "default".to_string(),
                    Arc::new(DatabaseEntry {
                        db,
                        sessions: SessionManager::new(),
                    }),
                );
            }
        } else {
            // In-memory mode: create a single default database
            tracing::info!("Creating default in-memory database");
            mgr.databases.insert(
                "default".to_string(),
                Arc::new(DatabaseEntry {
                    db: GrafeoDB::new_in_memory(),
                    sessions: SessionManager::new(),
                }),
            );
        }

        mgr
    }

    /// Returns a clone of the `Arc<DatabaseEntry>` for the given database name.
    pub fn get(&self, name: &str) -> Option<Arc<DatabaseEntry>> {
        self.databases.get(name).map(|e| Arc::clone(e.value()))
    }

    /// Creates a new named database. Returns error if name is invalid or already exists.
    pub fn create(&self, name: &str) -> Result<(), ApiError> {
        if !is_valid_name(name) {
            return Err(ApiError::BadRequest(format!(
                "invalid database name '{name}': must start with a letter, contain only \
                 alphanumeric/underscore/hyphen, and be at most 64 characters"
            )));
        }

        if self.databases.contains_key(name) {
            return Err(ApiError::Conflict(format!(
                "database '{name}' already exists"
            )));
        }

        let db = if let Some(ref dir) = self.data_dir {
            let db_dir = dir.join(name);
            std::fs::create_dir_all(&db_dir)
                .map_err(|e| ApiError::Internal(format!("failed to create directory: {e}")))?;
            let db_path = db_dir.join("grafeo.db");
            tracing::info!(name = %name, "Creating persistent database");
            GrafeoDB::open(db_path.to_str().unwrap())
                .map_err(|e| ApiError::Internal(format!("failed to create database: {e}")))?
        } else {
            tracing::info!(name = %name, "Creating in-memory database");
            GrafeoDB::new_in_memory()
        };

        self.databases.insert(
            name.to_string(),
            Arc::new(DatabaseEntry {
                db,
                sessions: SessionManager::new(),
            }),
        );

        Ok(())
    }

    /// Deletes a database by name. The `"default"` database cannot be deleted.
    pub fn delete(&self, name: &str) -> Result<(), ApiError> {
        if name == "default" {
            return Err(ApiError::BadRequest(
                "cannot delete the default database".to_string(),
            ));
        }

        let removed = self.databases.remove(name);
        if removed.is_none() {
            return Err(ApiError::NotFound(format!("database '{name}' not found")));
        }

        let (_, entry) = removed.unwrap();

        // Close the engine
        if let Err(e) = entry.db.close() {
            tracing::warn!(name = %name, error = %e, "Error closing database");
        }

        // Remove session mappings that pointed to this database
        self.session_db_map.retain(|_, db_name| db_name != name);

        // Remove on-disk data if persistent
        if let Some(ref dir) = self.data_dir {
            let db_dir = dir.join(name);
            if db_dir.exists()
                && let Err(e) = std::fs::remove_dir_all(&db_dir)
            {
                tracing::warn!(name = %name, error = %e, "Failed to remove database directory");
            }
        }

        tracing::info!(name = %name, "Database deleted");
        Ok(())
    }

    /// Lists all databases with summary info.
    pub fn list(&self) -> Vec<DatabaseSummary> {
        let mut result: Vec<DatabaseSummary> = self
            .databases
            .iter()
            .map(|entry| {
                let name = entry.key().clone();
                let db = &entry.value().db;
                DatabaseSummary {
                    name,
                    node_count: db.node_count(),
                    edge_count: db.edge_count(),
                    persistent: db.path().is_some(),
                }
            })
            .collect();
        result.sort_by(|a, b| a.name.cmp(&b.name));
        result
    }

    /// Returns the data directory, if configured.
    pub fn data_dir(&self) -> Option<&Path> {
        self.data_dir.as_deref()
    }

    /// Registers a session ID as belonging to a specific database.
    pub fn register_session(&self, session_id: &str, db_name: &str) {
        self.session_db_map
            .insert(session_id.to_string(), db_name.to_string());
    }

    /// Resolves which database a session belongs to.
    pub fn resolve_session(&self, session_id: &str) -> Option<String> {
        self.session_db_map
            .get(session_id)
            .map(|e| e.value().clone())
    }

    /// Removes a session from the session-to-database mapping.
    pub fn unregister_session(&self, session_id: &str) {
        self.session_db_map.remove(session_id);
    }

    /// Runs expired-session cleanup across all databases. Returns total removed.
    pub fn cleanup_all_expired(&self, ttl_secs: u64) -> usize {
        let mut total = 0;
        for entry in &self.databases {
            total += entry.value().sessions.cleanup_expired(ttl_secs);
        }
        // Also clean up session_db_map entries whose sessions no longer exist
        self.session_db_map.retain(|session_id, db_name| {
            if let Some(entry) = self.databases.get(db_name.as_str()) {
                // Check if session still exists by trying to get it (without touching TTL)
                // Since we just cleaned expired, if session is gone from manager it's expired
                entry.sessions.exists(session_id)
            } else {
                false
            }
        });
        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name_validation() {
        assert!(is_valid_name("default"));
        assert!(is_valid_name("my-db"));
        assert!(is_valid_name("my_db_123"));
        assert!(is_valid_name("A"));

        assert!(!is_valid_name(""));
        assert!(!is_valid_name("123abc")); // starts with digit
        assert!(!is_valid_name("-abc")); // starts with hyphen
        assert!(!is_valid_name("a b")); // space
        assert!(!is_valid_name("a".repeat(65).as_str())); // too long
    }

    #[test]
    fn test_in_memory_manager() {
        let mgr = DatabaseManager::new(None);
        assert!(mgr.get("default").is_some());

        // Create
        mgr.create("test").unwrap();
        assert!(mgr.get("test").is_some());

        // Duplicate
        assert!(mgr.create("test").is_err());

        // List
        let list = mgr.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "default");
        assert_eq!(list[1].name, "test");

        // Delete
        mgr.delete("test").unwrap();
        assert!(mgr.get("test").is_none());

        // Cannot delete default
        assert!(mgr.delete("default").is_err());
    }

    #[test]
    fn test_session_mapping() {
        let mgr = DatabaseManager::new(None);
        mgr.register_session("sess-1", "default");
        assert_eq!(mgr.resolve_session("sess-1").unwrap(), "default");

        mgr.unregister_session("sess-1");
        assert!(mgr.resolve_session("sess-1").is_none());
    }
}
