//! Multi-database registry.
//!
//! Each named database is an independent `GrafeoDB` instance with its own
//! session manager. The `"default"` database always exists and cannot be
//! deleted.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use grafeo_engine::{Config, DurabilityMode, GrafeoDB};
use serde::Serialize;

use crate::error::ApiError;
use crate::sessions::SessionManager;
use crate::types::{CreateDatabaseRequest, DatabaseType, StorageMode};

/// Default memory limit for new databases: 512 MB.
const DEFAULT_MEMORY_LIMIT: usize = 512 * 1024 * 1024;

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

/// Parses a WAL durability string into an engine `DurabilityMode`.
fn parse_durability(s: &str) -> Result<DurabilityMode, ApiError> {
    match s.to_lowercase().as_str() {
        "sync" => Ok(DurabilityMode::Sync),
        "batch" => Ok(DurabilityMode::default()), // Batch with default params
        "adaptive" => Ok(DurabilityMode::Adaptive {
            target_interval_ms: 100,
        }),
        "nosync" => Ok(DurabilityMode::NoSync),
        other => Err(ApiError::BadRequest(format!(
            "invalid wal_durability '{other}': expected \"sync\", \"batch\", \"adaptive\", or \"nosync\""
        ))),
    }
}

/// Creation-time metadata stored alongside each database.
#[derive(Clone)]
pub struct DatabaseMetadata {
    pub database_type: String,
    pub storage_mode: String,
    pub backward_edges: bool,
    pub threads: usize,
}

impl Default for DatabaseMetadata {
    fn default() -> Self {
        let num_cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        Self {
            database_type: "lpg".to_string(),
            storage_mode: "in-memory".to_string(),
            backward_edges: true,
            threads: num_cpus,
        }
    }
}

/// A single database instance with its own session manager and metadata.
pub struct DatabaseEntry {
    pub db: GrafeoDB,
    pub sessions: SessionManager,
    pub metadata: DatabaseMetadata,
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
#[derive(Serialize)]
#[cfg_attr(feature = "http", derive(utoipa::ToSchema))]
pub struct DatabaseSummary {
    /// Database name.
    pub name: String,
    /// Number of nodes.
    pub node_count: usize,
    /// Number of edges.
    pub edge_count: usize,
    /// Whether the database uses persistent storage.
    pub persistent: bool,
    /// Database type: "lpg", "rdf", "owl-schema", "rdfs-schema", "json-schema".
    pub database_type: String,
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
                                    let metadata = DatabaseMetadata {
                                        database_type: format!("{}", db.graph_model()),
                                        storage_mode: "persistent".to_string(),
                                        backward_edges: true,
                                        threads: std::thread::available_parallelism()
                                            .map(|n| n.get())
                                            .unwrap_or(1),
                                    };
                                    mgr.databases.insert(
                                        name,
                                        Arc::new(DatabaseEntry {
                                            db,
                                            sessions: SessionManager::new(),
                                            metadata,
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
                        metadata: DatabaseMetadata {
                            storage_mode: "persistent".to_string(),
                            ..Default::default()
                        },
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
                    metadata: DatabaseMetadata::default(),
                }),
            );
        }

        mgr
    }

    /// Returns a clone of the `Arc<DatabaseEntry>` for the given database name.
    pub fn get(&self, name: &str) -> Option<Arc<DatabaseEntry>> {
        self.databases.get(name).map(|e| Arc::clone(e.value()))
    }

    /// Creates a new named database from a full request. Returns error if name
    /// is invalid, already exists, or options are incompatible with server config.
    pub fn create(&self, req: &CreateDatabaseRequest) -> Result<(), ApiError> {
        let name = &req.name;

        if !is_valid_name(name) {
            return Err(ApiError::BadRequest(format!(
                "invalid database name '{name}': must start with a letter, contain only \
                 alphanumeric/underscore/hyphen, and be at most 64 characters"
            )));
        }

        if self.databases.contains_key(name.as_str()) {
            return Err(ApiError::Conflict(format!(
                "database '{name}' already exists"
            )));
        }

        // Validate: schema types require their feature flag
        #[cfg(not(feature = "owl-schema"))]
        if req.database_type == DatabaseType::OwlSchema {
            return Err(ApiError::BadRequest(
                "OWL Schema databases require the server to be compiled with the 'owl-schema' feature".to_string(),
            ));
        }
        #[cfg(not(feature = "rdfs-schema"))]
        if req.database_type == DatabaseType::RdfsSchema {
            return Err(ApiError::BadRequest(
                "RDFS Schema databases require the server to be compiled with the 'rdfs-schema' feature".to_string(),
            ));
        }
        #[cfg(not(feature = "json-schema"))]
        if req.database_type == DatabaseType::JsonSchema {
            return Err(ApiError::BadRequest(
                "JSON Schema databases require the server to be compiled with the 'json-schema' feature".to_string(),
            ));
        }

        // Validate: schema types require a schema file
        if req.database_type.requires_schema_file() && req.schema_file.is_none() {
            return Err(ApiError::BadRequest(format!(
                "database type '{}' requires a schema_file",
                req.database_type
            )));
        }

        // Build engine Config
        let graph_model = req.database_type.graph_model();
        let memory_limit = req
            .options
            .memory_limit_bytes
            .unwrap_or(DEFAULT_MEMORY_LIMIT);
        let backward_edges = req.options.backward_edges.unwrap_or(true);
        let threads = req.options.threads.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        });

        let mut config = match req.storage_mode {
            StorageMode::InMemory => Config::in_memory(),
            StorageMode::Persistent => {
                let dir = self.data_dir.as_ref().ok_or_else(|| {
                    ApiError::BadRequest(
                        "persistent storage requires the server to be started with --data-dir"
                            .to_string(),
                    )
                })?;
                let db_dir = dir.join(name.as_str());
                std::fs::create_dir_all(&db_dir)
                    .map_err(|e| ApiError::Internal(format!("failed to create directory: {e}")))?;
                let db_path = db_dir.join("grafeo.db");
                Config::persistent(db_path.to_str().unwrap())
            }
        };

        config = config
            .with_graph_model(graph_model)
            .with_memory_limit(memory_limit)
            .with_threads(threads);

        if !backward_edges {
            config = config.without_backward_edges();
        }

        if req.database_type == DatabaseType::JsonSchema {
            config = config.with_schema_constraints();
        }

        // WAL settings
        if let Some(wal_enabled) = req.options.wal_enabled
            && !wal_enabled
        {
            config.wal_enabled = false;
        }

        if let Some(ref durability_str) = req.options.wal_durability {
            config = config.with_wal_durability(parse_durability(durability_str)?);
        }

        if let Some(ref spill_path) = req.options.spill_path {
            config = config.with_spill_path(spill_path);
        }

        tracing::info!(
            name = %name,
            database_type = %req.database_type,
            storage_mode = ?req.storage_mode,
            memory_limit = memory_limit,
            "Creating database"
        );

        let db = GrafeoDB::with_config(config)
            .map_err(|e| ApiError::Internal(format!("failed to create database: {e}")))?;

        // Schema loading (if applicable)
        if let Some(ref schema_b64) = req.schema_file {
            let schema_bytes =
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, schema_b64)
                    .map_err(|e| {
                        ApiError::BadRequest(format!("invalid base64 in schema_file: {e}"))
                    })?;

            let schema_result = crate::schema::load_schema(req.database_type, &schema_bytes, &db);
            if let Err(e) = schema_result {
                // Rollback: close and remove the database
                let _ = db.close();
                if req.storage_mode == StorageMode::Persistent
                    && let Some(ref dir) = self.data_dir
                {
                    let db_dir = dir.join(name.as_str());
                    let _ = std::fs::remove_dir_all(db_dir);
                }
                return Err(e);
            }
        }

        let metadata = DatabaseMetadata {
            database_type: req.database_type.as_str().to_string(),
            storage_mode: req.storage_mode.as_str().to_string(),
            backward_edges,
            threads,
        };

        self.databases.insert(
            name.clone(),
            Arc::new(DatabaseEntry {
                db,
                sessions: SessionManager::new(),
                metadata,
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
                let e = entry.value();
                DatabaseSummary {
                    name,
                    node_count: e.db.node_count(),
                    edge_count: e.db.edge_count(),
                    persistent: e.db.path().is_some(),
                    database_type: e.metadata.database_type.clone(),
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

    /// Returns the total memory allocated across all databases.
    pub fn total_allocated_memory(&self) -> usize {
        self.databases
            .iter()
            .filter_map(|e| e.value().db.memory_limit())
            .sum()
    }

    /// Returns the total number of active transaction sessions across all databases.
    pub fn total_active_sessions(&self) -> usize {
        self.databases
            .iter()
            .map(|e| e.value().sessions.active_count())
            .sum()
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
    use crate::types::DatabaseOptions;

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

        // Create with minimal request (just name)
        let req = CreateDatabaseRequest {
            name: "test".to_string(),
            database_type: DatabaseType::Lpg,
            storage_mode: StorageMode::InMemory,
            options: DatabaseOptions::default(),
            schema_file: None,
            schema_filename: None,
        };
        mgr.create(&req).unwrap();
        assert!(mgr.get("test").is_some());

        // Check metadata
        let entry = mgr.get("test").unwrap();
        assert_eq!(entry.metadata.database_type, "lpg");
        assert_eq!(entry.metadata.storage_mode, "in-memory");

        // Duplicate
        assert!(mgr.create(&req).is_err());

        // List
        let list = mgr.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "default");
        assert_eq!(list[1].name, "test");
        assert_eq!(list[1].database_type, "lpg");

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

    #[test]
    fn test_persistent_rejected_without_data_dir() {
        let mgr = DatabaseManager::new(None);
        let req = CreateDatabaseRequest {
            name: "persist-test".to_string(),
            database_type: DatabaseType::Lpg,
            storage_mode: StorageMode::Persistent,
            options: DatabaseOptions::default(),
            schema_file: None,
            schema_filename: None,
        };
        let err = mgr.create(&req).unwrap_err();
        assert!(format!("{err:?}").contains("data-dir"));
    }
}
