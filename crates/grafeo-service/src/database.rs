//! Multi-database registry.
//!
//! Each named database is an independent `GrafeoDB` instance. The `"default"`
//! database always exists and cannot be deleted.
//!
//! Session management has been moved to `SessionRegistry` — this module only
//! handles database lifecycle (create/delete/list/info).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use arc_swap::ArcSwap;
use dashmap::DashMap;
use grafeo_engine::{Config, DurabilityMode, GrafeoDB};

use crate::error::ServiceError;
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
fn parse_durability(s: &str) -> Result<DurabilityMode, ServiceError> {
    match s.to_lowercase().as_str() {
        "sync" => Ok(DurabilityMode::Sync),
        "batch" => Ok(DurabilityMode::default()), // Batch with default params
        "adaptive" => Ok(DurabilityMode::Adaptive {
            target_interval_ms: 100,
        }),
        "nosync" => Ok(DurabilityMode::NoSync),
        other => Err(ServiceError::BadRequest(format!(
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
        let num_cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        Self {
            database_type: "lpg".to_string(),
            storage_mode: "in-memory".to_string(),
            backward_edges: true,
            threads: num_cpus,
        }
    }
}

/// Database availability state.
const STATE_AVAILABLE: u8 = 0;
const STATE_RESTORING: u8 = 1;

/// A single database instance with its metadata.
///
/// The `GrafeoDB` handle is behind an `ArcSwap` so that restore can hot-swap
/// the handle without removing the entry from the registry. Callers use
/// `entry.db()` to get a snapshot of the current handle.
pub struct DatabaseEntry {
    inner: ArcSwap<GrafeoDB>,
    state: AtomicU8,
    pub metadata: DatabaseMetadata,
}

impl std::fmt::Debug for DatabaseEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatabaseEntry")
            .field("inner", &"ArcSwap<GrafeoDB>")
            .field("state", &self.state.load(Ordering::Relaxed))
            .field("metadata", &self.metadata.database_type)
            .finish()
    }
}

impl DatabaseEntry {
    /// Creates a new entry in the `Available` state.
    pub fn new(db: Arc<GrafeoDB>, metadata: DatabaseMetadata) -> Self {
        Self {
            inner: ArcSwap::from(db),
            state: AtomicU8::new(STATE_AVAILABLE),
            metadata,
        }
    }

    /// Returns a snapshot of the current database handle.
    ///
    /// Lock-free on the read path. The returned `Arc` keeps the handle alive
    /// even if a concurrent restore swaps in a new one.
    pub fn db(&self) -> Arc<GrafeoDB> {
        self.inner.load_full()
    }

    /// Atomically swaps the database handle. Used by restore.
    pub fn swap_db(&self, new_db: Arc<GrafeoDB>) {
        self.inner.store(new_db);
    }

    /// Returns `true` if the database is currently being restored.
    pub fn is_restoring(&self) -> bool {
        self.state.load(Ordering::Acquire) == STATE_RESTORING
    }

    /// Marks the database as restoring. Returns `false` if already restoring.
    pub fn set_restoring(&self) -> bool {
        self.state
            .compare_exchange(
                STATE_AVAILABLE,
                STATE_RESTORING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Marks the database as available again.
    pub fn set_available(&self) {
        self.state.store(STATE_AVAILABLE, Ordering::Release);
    }

    /// Consumes the entry and returns the inner `Arc<GrafeoDB>` and metadata.
    ///
    /// Used by compact, which needs `Arc::get_mut` for `&mut GrafeoDB` access.
    /// The entry must not be in the DashMap when calling this.
    pub fn into_parts(self) -> (Arc<GrafeoDB>, DatabaseMetadata) {
        (self.inner.into_inner(), self.metadata)
    }
}

/// Thread-safe registry of named database instances.
pub struct DatabaseManager {
    databases: DashMap<String, Arc<DatabaseEntry>>,
    /// If `Some`, databases are persisted under `{data_dir}/{name}/data.grafeo`.
    data_dir: Option<PathBuf>,
    /// When `true`, reject all write operations.
    read_only: bool,
    /// When `true`, enable CDC on every database (needed for replication).
    #[cfg(feature = "cdc")]
    cdc_enabled: bool,
}

impl DatabaseManager {
    /// Creates a new manager. In persistent mode, scans `data_dir` for existing
    /// database subdirectories and opens each one. Ensures the `"default"` database
    /// always exists. Performs migration from the old single-file layout if needed.
    pub fn new(data_dir: Option<&str>, read_only: bool) -> Self {
        let mgr = Self {
            databases: DashMap::new(),
            data_dir: data_dir.map(PathBuf::from),
            read_only,
            #[cfg(feature = "cdc")]
            cdc_enabled: false,
        };

        if let Some(ref dir) = mgr.data_dir {
            std::fs::create_dir_all(dir).expect("failed to create data directory");

            // Migration: old flat layout had `{data_dir}/grafeo.db` directly.
            // Skip in read-only mode — can't rename on read-only mounts.
            let old_flat = dir.join("grafeo.db");
            if !mgr.read_only && old_flat.exists() {
                let new_dir = dir.join("default");
                std::fs::create_dir_all(&new_dir).expect("failed to create default db directory");
                let new_path = new_dir.join("data.grafeo");
                if !new_path.exists() {
                    tracing::info!("Migrating old flat layout to default/data.grafeo");
                    std::fs::rename(&old_flat, &new_path).expect("failed to migrate database file");
                    let old_wal = dir.join("grafeo.db.wal");
                    if old_wal.exists() {
                        let new_wal = new_dir.join("data.grafeo.wal");
                        let _ = std::fs::rename(&old_wal, &new_wal);
                    }
                }
            }

            // Read-only flat layout: open {data_dir}/grafeo.db directly as "default"
            if mgr.read_only && old_flat.exists() && !mgr.databases.contains_key("default") {
                tracing::info!("Opening legacy flat layout in read-only mode");
                match GrafeoDB::open_read_only(old_flat.to_str().unwrap()) {
                    Ok(db) => {
                        mgr.databases.insert(
                            "default".to_string(),
                            Arc::new(DatabaseEntry::new(
                                Arc::new(db),
                                DatabaseMetadata {
                                    storage_mode: "persistent".to_string(),
                                    ..Default::default()
                                },
                            )),
                        );
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to open legacy flat layout");
                    }
                }
            }

            // Scan subdirectories for existing databases
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        // Migrate legacy grafeo.db → data.grafeo (skip in read-only mode)
                        let legacy = path.join("grafeo.db");
                        let current = path.join("data.grafeo");
                        if !mgr.read_only && legacy.exists() && !current.exists() {
                            tracing::info!(path = %path.display(), "Migrating grafeo.db to data.grafeo");
                            let _ = std::fs::rename(&legacy, &current);
                            let legacy_wal = path.join("grafeo.db.wal");
                            if legacy_wal.exists() {
                                let _ = std::fs::rename(&legacy_wal, path.join("data.grafeo.wal"));
                            }
                        }

                        // Try data.grafeo first, fall back to legacy grafeo.db
                        let db_file = if current.exists() {
                            current
                        } else if legacy.exists() {
                            legacy
                        } else {
                            continue;
                        };
                        {
                            let name = entry.file_name().to_string_lossy().to_string();
                            tracing::info!(name = %name, read_only = mgr.read_only, "Opening database");
                            let open_result = if mgr.read_only {
                                GrafeoDB::open_read_only(db_file.to_str().unwrap())
                            } else {
                                GrafeoDB::open(db_file.to_str().unwrap())
                            };
                            match open_result {
                                Ok(db) => {
                                    let metadata = DatabaseMetadata {
                                        database_type: format!("{}", db.graph_model()),
                                        storage_mode: "persistent".to_string(),
                                        backward_edges: true,
                                        threads: std::thread::available_parallelism()
                                            .map_or(1, |n| n.get()),
                                    };
                                    mgr.databases.insert(
                                        name,
                                        Arc::new(DatabaseEntry::new(Arc::new(db), metadata)),
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
                let db_path = default_dir.join("data.grafeo");
                tracing::info!("Creating default persistent database");
                let db = if mgr.read_only {
                    GrafeoDB::open_read_only(db_path.to_str().unwrap())
                        .expect("failed to create default db")
                } else {
                    GrafeoDB::open(db_path.to_str().unwrap()).expect("failed to create default db")
                };
                mgr.databases.insert(
                    "default".to_string(),
                    Arc::new(DatabaseEntry::new(
                        Arc::new(db),
                        DatabaseMetadata {
                            storage_mode: "persistent".to_string(),
                            ..Default::default()
                        },
                    )),
                );
            }
        } else {
            // In-memory mode: create a single default database
            tracing::info!("Creating default in-memory database");
            mgr.databases.insert(
                "default".to_string(),
                Arc::new(DatabaseEntry::new(
                    Arc::new(GrafeoDB::new_in_memory()),
                    DatabaseMetadata::default(),
                )),
            );
        }

        mgr
    }

    /// Enables or disables CDC on all current databases and future ones.
    ///
    /// Called by `ServiceState` when the server is configured as a replication
    /// primary, so that all mutations generate CDC events for replicas to consume.
    #[cfg(feature = "cdc")]
    pub fn set_cdc_enabled(&mut self, enabled: bool) {
        self.cdc_enabled = enabled;
        for entry in &self.databases {
            entry.value().db().set_cdc_enabled(enabled);
        }
    }

    /// Returns a clone of the `Arc<DatabaseEntry>` for the given database name.
    pub fn get(&self, name: &str) -> Option<Arc<DatabaseEntry>> {
        self.databases.get(name).map(|e| Arc::clone(e.value()))
    }

    /// Returns a database entry, rejecting if it's currently being restored.
    ///
    /// Use this in request paths where a 503 is appropriate during restore.
    pub fn get_available(&self, name: &str) -> Result<Arc<DatabaseEntry>, ServiceError> {
        let entry = self
            .get(name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{name}' not found")))?;

        if entry.is_restoring() {
            return Err(ServiceError::Unavailable(format!(
                "database '{name}' is currently being restored"
            )));
        }

        Ok(entry)
    }

    /// Creates a new named database from a full request.
    pub fn create(&self, req: &CreateDatabaseRequest) -> Result<(), ServiceError> {
        if self.read_only {
            return Err(ServiceError::ReadOnly);
        }

        let name = &req.name;

        if !is_valid_name(name) {
            return Err(ServiceError::BadRequest(format!(
                "invalid database name '{name}': must start with a letter, contain only \
                 alphanumeric/underscore/hyphen, and be at most 64 characters"
            )));
        }

        if self.databases.contains_key(name.as_str()) {
            return Err(ServiceError::Conflict(format!(
                "database '{name}' already exists"
            )));
        }

        // Validate: schema types require their feature flag
        #[cfg(not(feature = "owl-schema"))]
        if req.database_type == DatabaseType::OwlSchema {
            return Err(ServiceError::BadRequest(
                "OWL Schema databases require the server to be compiled with the 'owl-schema' feature".to_string(),
            ));
        }
        #[cfg(not(feature = "rdfs-schema"))]
        if req.database_type == DatabaseType::RdfsSchema {
            return Err(ServiceError::BadRequest(
                "RDFS Schema databases require the server to be compiled with the 'rdfs-schema' feature".to_string(),
            ));
        }
        #[cfg(not(feature = "json-schema"))]
        if req.database_type == DatabaseType::JsonSchema {
            return Err(ServiceError::BadRequest(
                "JSON Schema databases require the server to be compiled with the 'json-schema' feature".to_string(),
            ));
        }

        // Validate: schema types require a schema file
        if req.database_type.requires_schema_file() && req.schema_file.is_none() {
            return Err(ServiceError::BadRequest(format!(
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
            std::thread::available_parallelism().map_or(1, |n| n.get())
        });

        let mut config = match req.storage_mode {
            StorageMode::InMemory => Config::in_memory(),
            StorageMode::Persistent => {
                let dir = self.data_dir.as_ref().ok_or_else(|| {
                    ServiceError::BadRequest(
                        "persistent storage requires the server to be started with --data-dir"
                            .to_string(),
                    )
                })?;
                let db_dir = dir.join(name.as_str());
                std::fs::create_dir_all(&db_dir).map_err(|e| {
                    ServiceError::Internal(format!("failed to create directory: {e}"))
                })?;
                let db_path = db_dir.join("data.grafeo");
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

        // Engine creation may fail if a recently-deleted database hasn't fully
        // released its resources yet (memory allocator, WAL flush, worker thread
        // join). Retry once after a brief pause to handle this race.
        let db = match GrafeoDB::with_config(config.clone()) {
            Ok(db) => db,
            Err(first_err) => {
                tracing::debug!(
                    name = %name,
                    error = %first_err,
                    "Engine creation failed, retrying after brief pause"
                );
                std::thread::sleep(std::time::Duration::from_millis(50));
                GrafeoDB::with_config(config).map_err(|e| {
                    ServiceError::Internal(format!("failed to create database after retry: {e}"))
                })?
            }
        };

        // Schema loading (if applicable)
        if let Some(ref schema_b64) = req.schema_file {
            let schema_bytes =
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, schema_b64)
                    .map_err(|e| {
                        ServiceError::BadRequest(format!("invalid base64 in schema_file: {e}"))
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

        // If CDC is enabled (replication primary), activate it on this database
        // so mutations produce change events for replicas.
        #[cfg(feature = "cdc")]
        if self.cdc_enabled {
            db.set_cdc_enabled(true);
        }

        let metadata = DatabaseMetadata {
            database_type: req.database_type.as_str().to_string(),
            storage_mode: req.storage_mode.as_str().to_string(),
            backward_edges,
            threads,
        };

        self.databases.insert(
            name.clone(),
            Arc::new(DatabaseEntry::new(Arc::new(db), metadata)),
        );

        Ok(())
    }

    /// Deletes a database by name. The `"default"` database cannot be deleted.
    ///
    /// The engine is closed and the `Arc<DatabaseEntry>` is explicitly dropped
    /// before any on-disk cleanup, ensuring that internal engine resources
    /// (memory allocator, WAL, worker threads) are fully released. This
    /// prevents resource contention if a new database is created immediately
    /// after deletion.
    pub fn delete(&self, name: &str) -> Result<(), ServiceError> {
        if self.read_only {
            return Err(ServiceError::ReadOnly);
        }

        if name == "default" {
            return Err(ServiceError::BadRequest(
                "cannot delete the default database".to_string(),
            ));
        }

        let removed = self.databases.remove(name);
        if removed.is_none() {
            return Err(ServiceError::NotFound(format!(
                "database '{name}' not found"
            )));
        }

        let (_, entry) = removed.unwrap();

        // Close the engine
        if let Err(e) = entry.db().close() {
            tracing::warn!(name = %name, error = %e, "Error closing database");
        }

        // Explicitly drop the Arc to ensure the engine is fully released
        // before we touch the filesystem. If other references exist (e.g.,
        // stale sessions), this won't be the final drop — but we've already
        // removed from the registry so new lookups will fail.
        drop(entry);

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
    pub fn list(&self) -> Vec<crate::types::DatabaseSummary> {
        let mut result: Vec<crate::types::DatabaseSummary> = self
            .databases
            .iter()
            .map(|entry| {
                let name = entry.key().clone();
                let e = entry.value();
                let db = e.db();
                crate::types::DatabaseSummary {
                    name,
                    node_count: db.node_count(),
                    edge_count: db.edge_count(),
                    persistent: db.path().is_some(),
                    database_type: e.metadata.database_type.clone(),
                }
            })
            .collect();
        result.sort_by(|a, b| a.name.cmp(&b.name));
        result
    }

    /// Removes a database entry for exclusive access (e.g. compaction).
    ///
    /// Returns the owned `DatabaseEntry` after verifying exclusive ownership
    /// of both the outer and inner Arcs. The entry is removed from the
    /// registry, so the caller **must** re-insert it via [`reinsert`] when
    /// done, even on failure.
    #[cfg(feature = "compact-store")]
    pub fn take_exclusive(&self, name: &str) -> Result<DatabaseEntry, ServiceError> {
        let (_, entry) = self
            .databases
            .remove(name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{name}' not found")))?;

        match Arc::try_unwrap(entry) {
            Ok(db_entry) => {
                // load_full() bumps the strong count by 1, so the expected
                // count when nobody else holds a reference is 2: one inside
                // the ArcSwap and one from this load_full() call.
                let db_arc = db_entry.db();
                if Arc::strong_count(&db_arc) > 2 {
                    drop(db_arc);
                    self.databases.insert(name.to_string(), Arc::new(db_entry));
                    return Err(ServiceError::Conflict(
                        "database is in use by active sessions, cannot compact".to_string(),
                    ));
                }
                drop(db_arc);
                Ok(db_entry)
            }
            Err(still_shared) => {
                self.databases.insert(name.to_string(), still_shared);
                Err(ServiceError::Conflict(
                    "database is in use by active sessions, cannot compact".to_string(),
                ))
            }
        }
    }

    /// Re-inserts a database entry previously removed via [`take_exclusive`].
    #[cfg(feature = "compact-store")]
    pub fn reinsert(&self, name: String, entry: DatabaseEntry) {
        self.databases.insert(name, Arc::new(entry));
    }

    /// Returns the data directory, if configured.
    pub fn data_dir(&self) -> Option<&Path> {
        self.data_dir.as_deref()
    }

    /// Returns whether the manager is in read-only mode.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Returns the total memory allocated across all databases.
    pub fn total_allocated_memory(&self) -> usize {
        self.databases
            .iter()
            .filter_map(|e| e.value().db().memory_limit())
            .sum()
    }

    /// Collects Prometheus metrics from all database engines.
    ///
    /// Each metric line is labelled with `database="<name>"` so that
    /// per-database breakdowns are available in Prometheus/Grafana.
    /// Returns `None` when the `metrics` engine feature is not compiled in.
    pub fn engine_prometheus_metrics(&self) -> Option<String> {
        #[cfg(feature = "metrics")]
        {
            let mut output = String::new();
            let mut seen_headers = std::collections::HashSet::new();
            for entry in &self.databases {
                let name = entry.key();
                let text = entry.value().db().metrics_prometheus();
                if text.is_empty() {
                    continue;
                }
                for line in text.lines() {
                    if line.starts_with("# ") {
                        // Deduplicate HELP/TYPE headers across databases
                        if seen_headers.insert(line.to_string()) {
                            output.push_str(line);
                            output.push('\n');
                        }
                    } else if line.is_empty() {
                        output.push('\n');
                    } else {
                        // Inject database label into metric lines
                        // e.g. "grafeo_query_count 42" -> "grafeo_query_count{database=\"default\"} 42"
                        // e.g. "grafeo_x{lang=\"gql\"} 1" -> "grafeo_x{database=\"default\",lang=\"gql\"} 1"
                        if let Some(brace_pos) = line.find('{') {
                            // Already has labels: insert database label after opening brace
                            output.push_str(&line[..=brace_pos]);
                            output.push_str("database=\"");
                            output.push_str(name);
                            output.push_str("\",");
                            output.push_str(&line[brace_pos + 1..]);
                        } else if let Some(space_pos) = line.find(' ') {
                            // No labels: insert {database="name"} before the space
                            output.push_str(&line[..space_pos]);
                            output.push('{');
                            output.push_str("database=\"");
                            output.push_str(name);
                            output.push_str("\"}");

                            output.push_str(&line[space_pos..]);
                        } else {
                            output.push_str(line);
                        }
                        output.push('\n');
                    }
                }
            }
            if output.is_empty() {
                None
            } else {
                Some(output)
            }
        }
        #[cfg(not(feature = "metrics"))]
        {
            None
        }
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
        let mgr = DatabaseManager::new(None, false);
        assert!(mgr.get("default").is_some());

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
    fn test_delete_then_recreate() {
        let mgr = DatabaseManager::new(None, false);
        let req = CreateDatabaseRequest {
            name: "ephemeral".to_string(),
            database_type: DatabaseType::Lpg,
            storage_mode: StorageMode::InMemory,
            options: DatabaseOptions::default(),
            schema_file: None,
            schema_filename: None,
        };

        // Create, delete, immediately recreate — exercises the close barrier
        mgr.create(&req).unwrap();
        assert!(mgr.get("ephemeral").is_some());

        mgr.delete("ephemeral").unwrap();
        assert!(mgr.get("ephemeral").is_none());

        // Should succeed without resource contention
        mgr.create(&req).unwrap();
        assert!(mgr.get("ephemeral").is_some());
    }

    #[test]
    fn test_persistent_rejected_without_data_dir() {
        let mgr = DatabaseManager::new(None, false);
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

    // -----------------------------------------------------------------------
    // DatabaseEntry state and ArcSwap
    // -----------------------------------------------------------------------

    #[test]
    fn entry_starts_available() {
        let db = Arc::new(GrafeoDB::new_in_memory());
        let entry = DatabaseEntry::new(db, DatabaseMetadata::default());
        assert!(!entry.is_restoring());
    }

    #[test]
    fn entry_set_restoring_and_available() {
        let db = Arc::new(GrafeoDB::new_in_memory());
        let entry = DatabaseEntry::new(db, DatabaseMetadata::default());

        assert!(entry.set_restoring());
        assert!(entry.is_restoring());
        // Double set returns false
        assert!(!entry.set_restoring());

        entry.set_available();
        assert!(!entry.is_restoring());
    }

    #[test]
    fn entry_swap_db() {
        let db1 = Arc::new(GrafeoDB::new_in_memory());
        let entry = DatabaseEntry::new(db1, DatabaseMetadata::default());
        let count_before = entry.db().node_count();

        let db2 = Arc::new(GrafeoDB::new_in_memory());
        entry.swap_db(db2);
        let count_after = entry.db().node_count();

        // Both empty, but the handle was swapped (no panic, correct behavior)
        assert_eq!(count_before, 0);
        assert_eq!(count_after, 0);
    }

    #[test]
    fn entry_into_parts() {
        let db = Arc::new(GrafeoDB::new_in_memory());
        let entry = DatabaseEntry::new(db, DatabaseMetadata::default());
        let (arc, meta) = entry.into_parts();
        assert_eq!(arc.node_count(), 0);
        assert_eq!(meta.database_type, "lpg");
    }

    #[test]
    fn entry_db_returns_handle() {
        let db = Arc::new(GrafeoDB::new_in_memory());
        let entry = DatabaseEntry::new(db, DatabaseMetadata::default());
        // load_full returns a usable Arc
        let handle = entry.db();
        assert_eq!(handle.node_count(), 0);
    }

    // -----------------------------------------------------------------------
    // get_available
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Persistent mode + migration
    // -----------------------------------------------------------------------

    #[test]
    fn persistent_manager_creates_data_grafeo() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DatabaseManager::new(Some(dir.path().to_str().unwrap()), false);
        assert!(mgr.get("default").is_some());
        // Should use data.grafeo, not grafeo.db
        assert!(dir.path().join("default").join("data.grafeo").exists());
        assert!(!dir.path().join("default").join("grafeo.db").exists());
    }

    #[test]
    fn persistent_manager_read_only() {
        let dir = tempfile::tempdir().unwrap();
        // First create a writable manager to set up the database
        {
            let _mgr = DatabaseManager::new(Some(dir.path().to_str().unwrap()), false);
        }
        // Now open in read-only mode
        let mgr = DatabaseManager::new(Some(dir.path().to_str().unwrap()), true);
        assert!(mgr.get("default").is_some());
        assert!(mgr.is_read_only());
    }

    #[test]
    fn migrate_grafeo_db_to_data_grafeo() {
        let dir = tempfile::tempdir().unwrap();
        let db_dir = dir.path().join("mydb");
        std::fs::create_dir_all(&db_dir).unwrap();

        // Create a database at the old path
        let old_path = db_dir.join("grafeo.db");
        {
            let db = GrafeoDB::open(old_path.to_str().unwrap()).unwrap();
            db.session().execute("INSERT (:Test {v: 1})").unwrap();
            db.close().ok();
        }
        assert!(old_path.exists());

        // Opening the manager should migrate it
        let mgr = DatabaseManager::new(Some(dir.path().to_str().unwrap()), false);

        // Old file should be gone, new file should exist
        assert!(!old_path.exists());
        assert!(db_dir.join("data.grafeo").exists());

        // Data should still be accessible
        let entry = mgr.get("mydb").unwrap();
        assert_eq!(entry.db().node_count(), 1);
    }

    #[test]
    fn migrate_grafeo_db_with_wal() {
        let dir = tempfile::tempdir().unwrap();
        let db_dir = dir.path().join("waldb");
        std::fs::create_dir_all(&db_dir).unwrap();

        // Create old database
        let old_path = db_dir.join("grafeo.db");
        {
            let db = GrafeoDB::open(old_path.to_str().unwrap()).unwrap();
            db.close().ok();
        }

        // Create a fake WAL sidecar
        let old_wal = db_dir.join("grafeo.db.wal");
        std::fs::create_dir_all(&old_wal).unwrap();
        std::fs::write(old_wal.join("wal_entry"), b"wal data").unwrap();

        let _mgr = DatabaseManager::new(Some(dir.path().to_str().unwrap()), false);

        // WAL should also be migrated
        assert!(!old_wal.exists());
        assert!(db_dir.join("data.grafeo.wal").exists());
    }

    #[test]
    fn migrate_flat_layout_to_default_subdir() {
        let dir = tempfile::tempdir().unwrap();

        // Create a database in the old flat layout (grafeo.db at root)
        let flat_path = dir.path().join("grafeo.db");
        {
            let db = GrafeoDB::open(flat_path.to_str().unwrap()).unwrap();
            db.session().execute("INSERT (:Flat {v: 42})").unwrap();
            db.close().ok();
        }

        let mgr = DatabaseManager::new(Some(dir.path().to_str().unwrap()), false);

        // Flat file should be gone, default subdir should have data.grafeo
        assert!(!flat_path.exists());
        assert!(dir.path().join("default").join("data.grafeo").exists());

        let entry = mgr.get("default").unwrap();
        assert_eq!(entry.db().node_count(), 1);
    }

    #[test]
    fn read_only_rejects_create() {
        let dir = tempfile::tempdir().unwrap();
        // Create writable first to set up default
        {
            let _mgr = DatabaseManager::new(Some(dir.path().to_str().unwrap()), false);
        }
        // Reopen read-only
        let mgr = DatabaseManager::new(Some(dir.path().to_str().unwrap()), true);
        let req = CreateDatabaseRequest {
            name: "new-db".to_string(),
            database_type: DatabaseType::Lpg,
            storage_mode: StorageMode::InMemory,
            options: DatabaseOptions::default(),
            schema_file: None,
            schema_filename: None,
        };
        assert!(matches!(mgr.create(&req), Err(ServiceError::ReadOnly)));
    }

    #[test]
    fn read_only_rejects_delete() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mgr = DatabaseManager::new(Some(dir.path().to_str().unwrap()), false);
            let req = CreateDatabaseRequest {
                name: "todelete".to_string(),
                database_type: DatabaseType::Lpg,
                storage_mode: StorageMode::Persistent,
                options: DatabaseOptions::default(),
                schema_file: None,
                schema_filename: None,
            };
            mgr.create(&req).unwrap();
        }
        let mgr = DatabaseManager::new(Some(dir.path().to_str().unwrap()), true);
        assert!(matches!(
            mgr.delete("todelete"),
            Err(ServiceError::ReadOnly)
        ));
    }

    #[test]
    fn read_only_preserves_data() {
        let dir = tempfile::tempdir().unwrap();
        // Write data
        {
            let mgr = DatabaseManager::new(Some(dir.path().to_str().unwrap()), false);
            let entry = mgr.get("default").unwrap();
            entry
                .db()
                .session()
                .execute("INSERT (:RoTest {v: 99})")
                .unwrap();
            assert_eq!(entry.db().node_count(), 1);
        }
        // Reopen read-only and verify data is preserved
        let mgr = DatabaseManager::new(Some(dir.path().to_str().unwrap()), true);
        let entry = mgr.get("default").unwrap();
        assert_eq!(entry.db().node_count(), 1);
    }

    // -----------------------------------------------------------------------
    // parse_durability
    // -----------------------------------------------------------------------

    #[test]
    fn parse_durability_all_modes() {
        assert!(matches!(parse_durability("sync"), Ok(DurabilityMode::Sync)));
        assert!(matches!(
            parse_durability("nosync"),
            Ok(DurabilityMode::NoSync)
        ));
        assert!(parse_durability("batch").is_ok());
        assert!(matches!(
            parse_durability("adaptive"),
            Ok(DurabilityMode::Adaptive { .. })
        ));
    }

    #[test]
    fn parse_durability_case_insensitive() {
        assert!(parse_durability("SYNC").is_ok());
        assert!(parse_durability("Batch").is_ok());
    }

    #[test]
    fn parse_durability_invalid() {
        let err = parse_durability("unknown").unwrap_err();
        assert!(matches!(err, ServiceError::BadRequest(_)));
    }

    #[test]
    fn get_available_returns_entry() {
        let mgr = DatabaseManager::new(None, false);
        let entry = mgr.get_available("default").unwrap();
        assert!(!entry.is_restoring());
    }

    #[test]
    fn get_available_not_found() {
        let mgr = DatabaseManager::new(None, false);
        let err = mgr.get_available("nonexistent").unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[test]
    fn get_available_returns_unavailable_when_restoring() {
        let mgr = DatabaseManager::new(None, false);
        let entry = mgr.get("default").unwrap();
        entry.set_restoring();

        let err = mgr.get_available("default").unwrap_err();
        assert!(matches!(err, ServiceError::Unavailable(_)));

        // Cleanup
        entry.set_available();
    }
}
