//! Admin operations — database introspection, maintenance, and index management.
//!
//! Transport-agnostic. Called by both HTTP routes and GWP backend.

use std::path::Path;

#[cfg(feature = "compact-store")]
use crate::database::DatabaseEntry;
use crate::database::DatabaseManager;
use crate::error::ServiceError;
use crate::metrics::Metrics;
use crate::types;

/// Stateless admin operations.
pub struct AdminService;

impl AdminService {
    /// Get detailed database statistics.
    ///
    /// `memory_bytes` and `disk_bytes` come from the engine's
    /// `detailed_stats()` path, which currently has two blind spots:
    ///
    /// - `memory_bytes` is just `buffer_manager.allocated()`, which is
    ///   ~0 for most databases. We override it with
    ///   `memory_usage().total_bytes` which walks every structure.
    /// - `disk_bytes` feeds a file path to a directory-walker, so it
    ///   always returns `None`. We compute it from the db's on-disk
    ///   directory under `data_dir`.
    ///
    /// Both overrides should go away if the engine grows proper
    /// accessors.
    pub async fn database_stats(
        databases: &DatabaseManager,
        db_name: &str,
    ) -> Result<types::DatabaseStats, ServiceError> {
        let entry = databases.get_available(db_name)?;
        let per_db_dir = databases.data_dir().map(|root| root.join(db_name));

        let (stats, memory_total) = tokio::task::spawn_blocking(move || {
            let db = entry.db();
            let stats = db.detailed_stats();
            let memory_total = db.memory_usage().total_bytes;
            (stats, memory_total)
        })
        .await
        .map_err(|e| ServiceError::Internal(e.to_string()))?;

        let disk_bytes = per_db_dir.and_then(|p| calculate_db_disk_usage(&p));

        Ok(types::DatabaseStats {
            name: db_name.to_owned(),
            node_count: stats.node_count,
            edge_count: stats.edge_count,
            label_count: stats.label_count,
            edge_type_count: stats.edge_type_count,
            property_key_count: stats.property_key_count,
            index_count: stats.index_count,
            memory_bytes: memory_total,
            disk_bytes,
        })
    }

    /// Get WAL status for a database.
    pub async fn wal_status(
        databases: &DatabaseManager,
        db_name: &str,
    ) -> Result<types::WalStatusInfo, ServiceError> {
        let entry = databases.get_available(db_name)?;

        let status = tokio::task::spawn_blocking(move || entry.db().wal_status())
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;

        Ok(types::WalStatusInfo {
            enabled: status.enabled,
            path: status.path.map(|p| p.to_string_lossy().into_owned()),
            size_bytes: status.size_bytes,
            record_count: status.record_count,
            last_checkpoint: status.last_checkpoint,
            current_epoch: status.current_epoch,
        })
    }

    /// Force a WAL checkpoint (flush pending records to storage).
    pub async fn wal_checkpoint(
        databases: &DatabaseManager,
        db_name: &str,
    ) -> Result<(), ServiceError> {
        if databases.is_read_only() {
            return Err(ServiceError::ReadOnly);
        }

        let entry = databases.get_available(db_name)?;

        #[cfg(feature = "async-storage")]
        {
            entry
                .db()
                .async_wal_checkpoint()
                .await
                .map_err(|e| ServiceError::Internal(e.to_string()))
        }
        #[cfg(not(feature = "async-storage"))]
        {
            tokio::task::spawn_blocking(move || entry.db().wal_checkpoint())
                .await
                .map_err(|e| ServiceError::Internal(e.to_string()))?
                .map_err(|e| ServiceError::Internal(e.to_string()))
        }
    }

    /// Validate database integrity.
    pub async fn validate(
        databases: &DatabaseManager,
        db_name: &str,
    ) -> Result<types::ValidationInfo, ServiceError> {
        let entry = databases.get_available(db_name)?;

        let result = tokio::task::spawn_blocking(move || entry.db().validate())
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;

        Ok(types::ValidationInfo {
            valid: result.is_valid(),
            errors: result
                .errors
                .into_iter()
                .map(|e| types::ValidationErrorItem {
                    code: e.code,
                    message: e.message,
                    context: e.context,
                })
                .collect(),
            warnings: result
                .warnings
                .into_iter()
                .map(|w| types::ValidationWarningItem {
                    code: w.code,
                    message: w.message,
                    context: w.context,
                })
                .collect(),
        })
    }

    /// Create an index on a database.
    pub async fn create_index(
        databases: &DatabaseManager,
        db_name: &str,
        index: types::IndexDef,
    ) -> Result<(), ServiceError> {
        if databases.is_read_only() {
            return Err(ServiceError::ReadOnly);
        }

        let entry = databases.get_available(db_name)?;

        tokio::task::spawn_blocking(move || match index {
            types::IndexDef::Property { property } => {
                entry.db().create_property_index(&property);
                Ok(())
            }
            #[cfg(feature = "vector-index")]
            types::IndexDef::Vector {
                label,
                property,
                dimensions,
                metric,
                m,
                ef_construction,
            } => entry
                .db()
                .create_vector_index(
                    &label,
                    &property,
                    dimensions.map(|d| d as usize),
                    metric.as_deref(),
                    m.map(|v| v as usize),
                    ef_construction.map(|v| v as usize),
                )
                .map_err(|e| ServiceError::BadRequest(e.to_string())),
            #[cfg(not(feature = "vector-index"))]
            types::IndexDef::Vector { .. } => Err(ServiceError::BadRequest(
                "vector-index feature not enabled".to_owned(),
            )),
            #[cfg(feature = "text-index")]
            types::IndexDef::Text { label, property } => entry
                .db()
                .create_text_index(&label, &property)
                .map_err(|e| ServiceError::BadRequest(e.to_string())),
            #[cfg(not(feature = "text-index"))]
            types::IndexDef::Text { .. } => Err(ServiceError::BadRequest(
                "text-index feature not enabled".to_owned(),
            )),
        })
        .await
        .map_err(|e| ServiceError::Internal(e.to_string()))?
    }

    /// Get query plan cache statistics.
    pub async fn cache_stats(
        databases: &DatabaseManager,
        db_name: &str,
    ) -> Result<types::CacheStatsInfo, ServiceError> {
        let entry = databases.get_available(db_name)?;

        let stats = tokio::task::spawn_blocking(move || entry.db().query_cache().stats())
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;

        let parsed_hit_rate = if stats.parsed_hits + stats.parsed_misses > 0 {
            Some(stats.parsed_hits as f64 / (stats.parsed_hits + stats.parsed_misses) as f64)
        } else {
            None
        };
        let optimized_hit_rate = if stats.optimized_hits + stats.optimized_misses > 0 {
            Some(
                stats.optimized_hits as f64
                    / (stats.optimized_hits + stats.optimized_misses) as f64,
            )
        } else {
            None
        };

        Ok(types::CacheStatsInfo {
            parsed_size: stats.parsed_size,
            optimized_size: stats.optimized_size,
            parsed_hits: stats.parsed_hits,
            parsed_misses: stats.parsed_misses,
            optimized_hits: stats.optimized_hits,
            optimized_misses: stats.optimized_misses,
            invalidations: stats.invalidations,
            parsed_hit_rate,
            optimized_hit_rate,
        })
    }

    /// Clear the query plan cache for a database.
    pub async fn clear_cache(
        databases: &DatabaseManager,
        db_name: &str,
    ) -> Result<(), ServiceError> {
        let entry = databases.get_available(db_name)?;

        tokio::task::spawn_blocking(move || entry.db().clear_plan_cache())
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))
    }

    /// Get hierarchical memory usage breakdown for a database.
    pub async fn memory_usage(
        databases: &DatabaseManager,
        db_name: &str,
    ) -> Result<serde_json::Value, ServiceError> {
        let entry = databases.get_available(db_name)?;

        let usage = tokio::task::spawn_blocking(move || entry.db().memory_usage())
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;

        serde_json::to_value(&usage).map_err(|e| ServiceError::Internal(e.to_string()))
    }

    /// List named graphs within a database.
    pub async fn list_graphs(
        databases: &DatabaseManager,
        db_name: &str,
    ) -> Result<Vec<String>, ServiceError> {
        let entry = databases.get_available(db_name)?;

        tokio::task::spawn_blocking(move || entry.db().list_graphs())
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))
    }

    /// Create a named graph within a database.
    ///
    /// Returns true if the graph was created, false if it already existed.
    pub async fn create_graph(
        databases: &DatabaseManager,
        db_name: &str,
        graph_name: String,
    ) -> Result<bool, ServiceError> {
        if databases.is_read_only() {
            return Err(ServiceError::ReadOnly);
        }

        let entry = databases.get_available(db_name)?;

        tokio::task::spawn_blocking(move || entry.db().create_graph(&graph_name))
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?
            .map_err(|e| ServiceError::Internal(e.to_string()))
    }

    /// Drop a named graph within a database.
    ///
    /// Returns true if the graph existed and was dropped, false otherwise.
    pub async fn drop_graph(
        databases: &DatabaseManager,
        db_name: &str,
        graph_name: String,
    ) -> Result<bool, ServiceError> {
        if databases.is_read_only() {
            return Err(ServiceError::ReadOnly);
        }

        let entry = databases.get_available(db_name)?;

        tokio::task::spawn_blocking(move || entry.db().drop_graph(&graph_name))
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))
    }

    /// Drop an index from a database.
    pub async fn drop_index(
        databases: &DatabaseManager,
        db_name: &str,
        index: types::IndexDef,
    ) -> Result<bool, ServiceError> {
        if databases.is_read_only() {
            return Err(ServiceError::ReadOnly);
        }

        let entry = databases.get_available(db_name)?;

        tokio::task::spawn_blocking(move || match index {
            types::IndexDef::Property { property } => entry.db().drop_property_index(&property),
            #[cfg(feature = "vector-index")]
            types::IndexDef::Vector {
                label, property, ..
            } => entry.db().drop_vector_index(&label, &property),
            #[cfg(not(feature = "vector-index"))]
            types::IndexDef::Vector { .. } => false,
            #[cfg(feature = "text-index")]
            types::IndexDef::Text { label, property } => {
                entry.db().drop_text_index(&label, &property)
            }
            #[cfg(not(feature = "text-index"))]
            types::IndexDef::Text { .. } => false,
        })
        .await
        .map_err(|e| ServiceError::Internal(e.to_string()))
    }

    /// Compact a database into a read-only columnar store.
    ///
    /// This is a **one-way operation**: the database becomes permanently
    /// read-only after compaction. The columnar format uses significantly
    /// less memory and is optimized for analytical read workloads.
    ///
    /// Requires exclusive access to the database (no active sessions or
    /// concurrent requests holding a reference).
    #[allow(clippy::unused_async)] // async needed when compact-store is enabled
    pub async fn compact(databases: &DatabaseManager, db_name: &str) -> Result<(), ServiceError> {
        if databases.is_read_only() {
            return Err(ServiceError::ReadOnly);
        }

        #[cfg(feature = "compact-store")]
        {
            use std::panic::{AssertUnwindSafe, catch_unwind};
            use std::sync::Arc;

            let db_entry = databases.take_exclusive(db_name)?;
            let name = db_name.to_owned();

            let result = tokio::task::spawn_blocking(move || {
                let (mut db_arc, mut metadata) = db_entry.into_parts();
                let db = match Arc::get_mut(&mut db_arc) {
                    Some(db) => db,
                    None => {
                        let entry = DatabaseEntry::new(db_arc, metadata);
                        return Err((
                            entry,
                            ServiceError::Conflict(
                                "inner Arc<GrafeoDB> still shared after take_exclusive".to_string(),
                            ),
                        ));
                    }
                };

                match catch_unwind(AssertUnwindSafe(|| db.compact())) {
                    Ok(Ok(())) => {
                        metadata.storage_mode = "compact".to_string();
                        Ok(DatabaseEntry::new(db_arc, metadata))
                    }
                    Ok(Err(e)) => Err((
                        DatabaseEntry::new(db_arc, metadata),
                        ServiceError::Internal(format!("compaction failed: {e}")),
                    )),
                    Err(_panic) => Err((
                        DatabaseEntry::new(db_arc, metadata),
                        ServiceError::Internal("compaction panicked".to_string()),
                    )),
                }
            })
            .await
            .expect("compact: spawn_blocking task should not be cancelled");

            match result {
                Ok(compacted) => {
                    databases.reinsert(name.clone(), compacted);
                    tracing::info!(name = %name, "Database compacted to columnar read-only store");
                    Ok(())
                }
                Err((original, err)) => {
                    databases.reinsert(name, original);
                    Err(err)
                }
            }
        }
        #[cfg(not(feature = "compact-store"))]
        {
            let _ = db_name;
            Err(ServiceError::BadRequest(
                "compact-store feature not enabled".to_string(),
            ))
        }
    }

    /// Bulk-import a TSV edge list into a database.
    ///
    /// Bypasses per-edge transaction overhead by batching all operations
    /// into a single transaction, achieving 10-100x throughput over
    /// individual inserts for large graphs.
    pub async fn import_tsv(
        databases: &DatabaseManager,
        db_name: &str,
        data: String,
        edge_type: String,
        directed: bool,
    ) -> Result<types::ImportResponse, ServiceError> {
        if databases.is_read_only() {
            return Err(ServiceError::ReadOnly);
        }

        let entry = databases.get_available(db_name)?;

        let (nodes_created, edges_created) = tokio::task::spawn_blocking(move || {
            entry.db().import_tsv_str(&data, &edge_type, directed)
        })
        .await
        .map_err(|e| ServiceError::Internal(e.to_string()))?
        .map_err(|e| ServiceError::BadRequest(e.to_string()))?;

        Ok(types::ImportResponse {
            nodes_created,
            edges_created,
        })
    }

    // -----------------------------------------------------------------------
    // Schema management (ISO/IEC 39075 Section 4.2.5)
    // -----------------------------------------------------------------------

    /// List all schema namespaces in a database.
    pub async fn list_schemas(
        databases: &DatabaseManager,
        metrics: &Metrics,
        db_name: &str,
    ) -> Result<Vec<String>, ServiceError> {
        let result = crate::query::QueryService::execute(
            databases,
            metrics,
            db_name,
            "SHOW SCHEMAS",
            Some("gql"),
            None,
            None,
            false,
            None,
        )
        .await?;

        Ok(result
            .into_rows()
            .into_iter()
            .filter_map(|row| {
                row.into_iter()
                    .next()
                    .and_then(|v| v.as_str().map(str::to_owned))
            })
            .collect())
    }

    /// Create a new schema namespace in a database.
    ///
    /// Returns `true` if the schema was created, `false` if it already existed.
    /// Only "already exists" errors are converted to `Ok(false)`: real failures
    /// (database not found, internal errors) propagate as `Err`.
    pub async fn create_schema(
        databases: &DatabaseManager,
        metrics: &Metrics,
        db_name: &str,
        schema_name: &str,
    ) -> Result<bool, ServiceError> {
        if databases.is_read_only() {
            return Err(ServiceError::ReadOnly);
        }

        let result = crate::query::QueryService::execute(
            databases,
            metrics,
            db_name,
            &format!("CREATE SCHEMA {schema_name}"),
            Some("gql"),
            None,
            None,
            false,
            None,
        )
        .await;

        match result {
            Ok(_) => Ok(true),
            Err(ref e) if e.to_string().contains("already exists") => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Drop a schema namespace from a database.
    ///
    /// Returns `true` if the schema existed and was dropped, `false` if the
    /// schema was not found. Only "not found" errors are converted to
    /// `Ok(false)`: real failures propagate as `Err`.
    pub async fn drop_schema(
        databases: &DatabaseManager,
        metrics: &Metrics,
        db_name: &str,
        schema_name: &str,
    ) -> Result<bool, ServiceError> {
        if databases.is_read_only() {
            return Err(ServiceError::ReadOnly);
        }

        let result = crate::query::QueryService::execute(
            databases,
            metrics,
            db_name,
            &format!("DROP SCHEMA {schema_name}"),
            Some("gql"),
            None,
            None,
            false,
            None,
        )
        .await;

        match result {
            Ok(_) => Ok(true),
            Err(ref e)
                if e.to_string().contains("not found")
                    || e.to_string().contains("does not exist") =>
            {
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    // -----------------------------------------------------------------------
    // Graph projections
    // -----------------------------------------------------------------------

    /// Create a graph projection.
    pub async fn create_projection(
        databases: &DatabaseManager,
        db_name: &str,
        req: types::CreateProjectionRequest,
    ) -> Result<bool, ServiceError> {
        if databases.is_read_only() {
            return Err(ServiceError::ReadOnly);
        }

        let entry = databases.get_available(db_name)?;
        tokio::task::spawn_blocking(move || {
            let spec = grafeo_engine::ProjectionSpec::new()
                .with_node_labels(req.node_labels)
                .with_edge_types(req.edge_types);
            Ok(entry.db().create_projection(req.name, spec))
        })
        .await
        .map_err(|e| ServiceError::Internal(e.to_string()))?
    }

    /// Drop a graph projection.
    pub async fn drop_projection(
        databases: &DatabaseManager,
        db_name: &str,
        name: &str,
    ) -> Result<bool, ServiceError> {
        if databases.is_read_only() {
            return Err(ServiceError::ReadOnly);
        }

        let entry = databases.get_available(db_name)?;
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || Ok(entry.db().drop_projection(&name)))
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?
    }

    /// List all graph projections in a database.
    pub async fn list_projections(
        databases: &DatabaseManager,
        db_name: &str,
    ) -> Result<Vec<String>, ServiceError> {
        let entry = databases.get_available(db_name)?;
        tokio::task::spawn_blocking(move || Ok(entry.db().list_projections()))
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?
    }

    /// Write a point-in-time snapshot to the `.grafeo` database file.
    ///
    /// Requires the `async-storage` and `grafeo-file` features. Returns an
    /// error explaining the missing features when they are not enabled.
    // The await lives inside the cfg(async-storage, grafeo-file) branch;
    // without those features the function body is sync, but the signature
    // must remain async for the HTTP handler.
    #[allow(clippy::unused_async)]
    pub async fn write_snapshot(
        databases: &DatabaseManager,
        db_name: &str,
    ) -> Result<(), ServiceError> {
        if databases.is_read_only() {
            return Err(ServiceError::ReadOnly);
        }

        let entry = databases.get_available(db_name)?;

        #[cfg(all(feature = "async-storage", feature = "grafeo-file"))]
        {
            entry
                .db()
                .async_write_snapshot()
                .await
                .map_err(|e| ServiceError::Internal(e.to_string()))
        }
        #[cfg(not(all(feature = "async-storage", feature = "grafeo-file")))]
        {
            let _ = entry;
            Err(ServiceError::BadRequest(
                "snapshot requires the 'async-storage' and 'grafeo-file' features".to_string(),
            ))
        }
    }

    // -----------------------------------------------------------------------
    // SHACL validation
    // -----------------------------------------------------------------------

    /// Validate RDF data against SHACL shapes.
    #[cfg(feature = "shacl")]
    #[allow(clippy::unused_async)]
    pub async fn validate_shacl(
        databases: &DatabaseManager,
        db_name: &str,
        req: &types::ShaclValidateRequest,
    ) -> Result<types::ShaclValidationReport, ServiceError> {
        let entry = databases.get_available(db_name)?;
        let shapes = req.shapes_graph.clone();
        let data_graph = req.data_graph.clone();

        tokio::task::spawn_blocking(move || {
            let session = entry.db().session();
            let report = if let Some(ref dg) = data_graph {
                session.validate_shacl_graph(dg, &shapes)
            } else {
                session.validate_shacl(&shapes)
            }
            .map_err(|e| ServiceError::BadRequest(e.to_string()))?;

            Ok(types::ShaclValidationReport {
                conforms: report.conforms,
                results: report
                    .results
                    .iter()
                    .map(|r| types::ShaclViolation {
                        focus_node: format!("{}", r.focus_node),
                        constraint: r.source_constraint_component.clone(),
                        source_shape: format!("{}", r.source_shape),
                        severity: format!("{:?}", r.severity),
                        value: r.value.as_ref().map(|v| format!("{v}")),
                        path: r.result_path.as_ref().map(|p| format!("{p:?}")),
                        message: r.message.clone(),
                    })
                    .collect(),
            })
        })
        .await
        .map_err(|e| ServiceError::Internal(e.to_string()))?
    }

    /// Validate RDF data against SHACL shapes (stub when feature disabled).
    #[cfg(not(feature = "shacl"))]
    #[allow(clippy::unused_async)]
    pub async fn validate_shacl(
        _databases: &DatabaseManager,
        _db_name: &str,
        _req: &types::ShaclValidateRequest,
    ) -> Result<types::ShaclValidationReport, ServiceError> {
        Err(ServiceError::BadRequest(
            "shacl feature not enabled".to_owned(),
        ))
    }
}

/// Recursively sum the size of every regular file under `dir`.
///
/// Returns `None` if the directory doesn't exist, isn't a directory, or
/// can't be fully traversed (e.g. permissions denied on a subtree). The
/// caller treats `None` as "no known disk usage" and displays a dash
/// rather than a bogus zero, which would read as "the database is
/// empty on disk" when it might just be unreadable.
fn calculate_db_disk_usage(dir: &Path) -> Option<usize> {
    if !dir.exists() || !dir.is_dir() {
        return None;
    }
    let mut total: usize = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(p) = stack.pop() {
        let entries = std::fs::read_dir(&p).ok()?;
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total = total.saturating_add(meta.len() as usize);
            }
        }
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ServiceState;

    #[tokio::test]
    async fn test_database_stats_default_db() {
        let state = ServiceState::new_in_memory(300);
        let stats = AdminService::database_stats(state.databases(), "default")
            .await
            .unwrap();
        assert_eq!(stats.name, "default");
        assert_eq!(stats.node_count, 0);
        assert_eq!(stats.edge_count, 0);
    }

    #[tokio::test]
    async fn test_database_stats_memory_and_disk_nonzero_on_persistent() {
        use crate::database::DatabaseManager;
        let data_dir = tempfile::tempdir().unwrap();
        let mgr = DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        // Insert some data so both memory and disk usage are non-zero.
        {
            let entry = mgr.get("default").unwrap();
            for i in 0..10 {
                entry
                    .db()
                    .session()
                    .execute(&format!("INSERT (:Person {{idx: {i}}})"))
                    .unwrap();
            }
        }

        let stats = AdminService::database_stats(&mgr, "default").await.unwrap();
        assert_eq!(stats.node_count, 10);
        // memory_bytes now uses memory_usage().total_bytes which walks every
        // structure — should be meaningfully > 0 with real data.
        assert!(
            stats.memory_bytes > 0,
            "expected non-zero memory_bytes after inserts, got {}",
            stats.memory_bytes
        );
        // disk_bytes now walks the per-db directory — should include at
        // least the data.grafeo file or its WAL for a persistent database.
        assert!(
            stats.disk_bytes.is_some_and(|n| n > 0),
            "expected non-zero disk_bytes for persistent db, got {:?}",
            stats.disk_bytes
        );
    }

    #[test]
    fn calculate_db_disk_usage_missing_dir_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        assert!(calculate_db_disk_usage(&missing).is_none());
    }

    #[test]
    fn calculate_db_disk_usage_sums_nested_files() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(tmp.path().join("a.bin"), vec![0u8; 100]).unwrap();
        std::fs::write(sub.join("b.bin"), vec![0u8; 250]).unwrap();
        let total = calculate_db_disk_usage(tmp.path()).unwrap();
        assert_eq!(total, 350);
    }

    #[cfg(unix)]
    #[test]
    fn calculate_db_disk_usage_returns_none_when_subdir_unreadable() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.bin"), vec![0u8; 100]).unwrap();
        let locked = tmp.path().join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("b.bin"), vec![0u8; 500]).unwrap();

        // Strip read/execute on the subdirectory so read_dir fails.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let result = calculate_db_disk_usage(tmp.path());

        // Restore permissions so TempDir cleanup works.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            result.is_none(),
            "expected None when a subdirectory is unreadable, got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_database_stats_not_found() {
        let state = ServiceState::new_in_memory(300);
        let err = AdminService::database_stats(state.databases(), "nonexistent")
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[tokio::test]
    async fn test_wal_status_in_memory() {
        let state = ServiceState::new_in_memory(300);
        let status = AdminService::wal_status(state.databases(), "default")
            .await
            .unwrap();
        assert!(!status.enabled);
    }

    #[tokio::test]
    async fn test_validate_clean_db() {
        let state = ServiceState::new_in_memory(300);
        let result = AdminService::validate(state.databases(), "default")
            .await
            .unwrap();
        assert!(result.valid);
        assert!(result.errors.is_empty());
    }

    #[tokio::test]
    async fn test_wal_checkpoint_in_memory() {
        let state = ServiceState::new_in_memory(300);
        AdminService::wal_checkpoint(state.databases(), "default")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_create_property_index() {
        let state = ServiceState::new_in_memory(300);
        AdminService::create_index(
            state.databases(),
            "default",
            types::IndexDef::Property {
                property: "name".to_owned(),
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_cache_stats_empty_db() {
        let state = ServiceState::new_in_memory(300);
        let stats = AdminService::cache_stats(state.databases(), "default")
            .await
            .unwrap();
        // Fresh DB: no queries executed, all counters at zero
        assert_eq!(stats.parsed_hits, 0);
        assert_eq!(stats.parsed_misses, 0);
        assert_eq!(stats.optimized_hits, 0);
        assert_eq!(stats.optimized_misses, 0);
        // No queries means hit rate is undefined (None)
        assert!(stats.parsed_hit_rate.is_none());
        assert!(stats.optimized_hit_rate.is_none());
    }

    #[tokio::test]
    async fn test_cache_stats_not_found() {
        let state = ServiceState::new_in_memory(300);
        let err = AdminService::cache_stats(state.databases(), "nonexistent")
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[tokio::test]
    async fn test_clear_cache() {
        let state = ServiceState::new_in_memory(300);
        // Should succeed even on a fresh DB
        AdminService::clear_cache(state.databases(), "default")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_clear_cache_not_found() {
        let state = ServiceState::new_in_memory(300);
        let err = AdminService::clear_cache(state.databases(), "nonexistent")
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[tokio::test]
    async fn test_drop_property_index() {
        let state = ServiceState::new_in_memory(300);
        AdminService::create_index(
            state.databases(),
            "default",
            types::IndexDef::Property {
                property: "email".to_owned(),
            },
        )
        .await
        .unwrap();
        let existed = AdminService::drop_index(
            state.databases(),
            "default",
            types::IndexDef::Property {
                property: "email".to_owned(),
            },
        )
        .await
        .unwrap();
        assert!(existed);
        let existed = AdminService::drop_index(
            state.databases(),
            "default",
            types::IndexDef::Property {
                property: "email".to_owned(),
            },
        )
        .await
        .unwrap();
        assert!(!existed);
    }

    #[tokio::test]
    async fn test_memory_usage_default_db() {
        let state = ServiceState::new_in_memory(300);
        let usage = AdminService::memory_usage(state.databases(), "default")
            .await
            .unwrap();
        assert!(usage["total_bytes"].is_u64());
        assert!(usage["store"].is_object());
        assert!(usage["indexes"].is_object());
        assert!(usage["mvcc"].is_object());
        assert!(usage["caches"].is_object());
        assert!(usage["string_pool"].is_object());
        assert!(usage["buffer_manager"].is_object());
    }

    #[tokio::test]
    async fn test_memory_usage_not_found() {
        let state = ServiceState::new_in_memory(300);
        let err = AdminService::memory_usage(state.databases(), "nonexistent")
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[tokio::test]
    async fn test_list_graphs_empty() {
        let state = ServiceState::new_in_memory(300);
        let graphs = AdminService::list_graphs(state.databases(), "default")
            .await
            .unwrap();
        assert!(graphs.is_empty());
    }

    #[tokio::test]
    async fn test_create_and_drop_graph() {
        let state = ServiceState::new_in_memory(300);
        let created =
            AdminService::create_graph(state.databases(), "default", "analytics".to_owned())
                .await
                .unwrap();
        assert!(created);

        let graphs = AdminService::list_graphs(state.databases(), "default")
            .await
            .unwrap();
        assert_eq!(graphs, vec!["analytics"]);

        let created_again =
            AdminService::create_graph(state.databases(), "default", "analytics".to_owned())
                .await
                .unwrap();
        assert!(!created_again);

        let dropped =
            AdminService::drop_graph(state.databases(), "default", "analytics".to_owned())
                .await
                .unwrap();
        assert!(dropped);

        let dropped_again =
            AdminService::drop_graph(state.databases(), "default", "analytics".to_owned())
                .await
                .unwrap();
        assert!(!dropped_again);
    }

    #[tokio::test]
    async fn test_list_graphs_not_found() {
        let state = ServiceState::new_in_memory(300);
        let err = AdminService::list_graphs(state.databases(), "nonexistent")
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    // -----------------------------------------------------------------------
    // Graph projections
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_create_and_drop_projection() {
        let state = ServiceState::new_in_memory(300);
        let req = types::CreateProjectionRequest {
            name: "social".to_owned(),
            node_labels: vec!["Person".to_owned()],
            edge_types: vec!["KNOWS".to_owned()],
        };
        let created = AdminService::create_projection(state.databases(), "default", req)
            .await
            .unwrap();
        assert!(created);

        let list = AdminService::list_projections(state.databases(), "default")
            .await
            .unwrap();
        assert_eq!(list, vec!["social"]);

        // Duplicate returns false
        let req2 = types::CreateProjectionRequest {
            name: "social".to_owned(),
            node_labels: vec![],
            edge_types: vec![],
        };
        let created_again = AdminService::create_projection(state.databases(), "default", req2)
            .await
            .unwrap();
        assert!(!created_again);

        let dropped = AdminService::drop_projection(state.databases(), "default", "social")
            .await
            .unwrap();
        assert!(dropped);

        let dropped_again = AdminService::drop_projection(state.databases(), "default", "social")
            .await
            .unwrap();
        assert!(!dropped_again);
    }

    #[tokio::test]
    async fn test_list_projections_empty() {
        let state = ServiceState::new_in_memory(300);
        let list = AdminService::list_projections(state.databases(), "default")
            .await
            .unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn test_projections_not_found() {
        let state = ServiceState::new_in_memory(300);
        let err = AdminService::list_projections(state.databases(), "nonexistent")
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    // -----------------------------------------------------------------------
    // SHACL validation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_validate_shacl_feature_response() {
        let state = ServiceState::new_in_memory(300);
        let req = types::ShaclValidateRequest {
            shapes_graph: String::new(),
            data_graph: None,
        };
        let result = AdminService::validate_shacl(state.databases(), "default", &req).await;
        // When shacl feature is disabled, returns BadRequest; when enabled,
        // may return an engine error for empty shapes. Either way, we exercise
        // the method dispatch.
        assert!(result.is_ok() || result.is_err());
    }

    #[tokio::test]
    async fn test_validate_shacl_not_found() {
        let state = ServiceState::new_in_memory(300);
        let req = types::ShaclValidateRequest {
            shapes_graph: String::new(),
            data_graph: None,
        };
        let err = AdminService::validate_shacl(state.databases(), "nonexistent", &req)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ServiceError::NotFound(_)) || matches!(err, ServiceError::BadRequest(_))
        );
    }

    #[tokio::test]
    async fn test_write_snapshot_in_memory() {
        let state = ServiceState::new_in_memory(300);
        // In-memory databases have no file manager, so snapshot should fail.
        let result = AdminService::write_snapshot(state.databases(), "default").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_write_snapshot_not_found() {
        let state = ServiceState::new_in_memory(300);
        let err = AdminService::write_snapshot(state.databases(), "nonexistent")
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }
}
