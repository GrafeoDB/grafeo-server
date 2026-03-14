//! Admin operations — database introspection, maintenance, and index management.
//!
//! Transport-agnostic. Called by both HTTP routes and GWP backend.

use crate::database::DatabaseManager;
use crate::error::ServiceError;
use crate::types;

/// Stateless admin operations.
pub struct AdminService;

impl AdminService {
    /// Get detailed database statistics.
    pub async fn database_stats(
        databases: &DatabaseManager,
        db_name: &str,
    ) -> Result<types::DatabaseStats, ServiceError> {
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        let stats = tokio::task::spawn_blocking(move || entry.db.detailed_stats())
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;

        Ok(types::DatabaseStats {
            name: db_name.to_owned(),
            node_count: stats.node_count,
            edge_count: stats.edge_count,
            label_count: stats.label_count,
            edge_type_count: stats.edge_type_count,
            property_key_count: stats.property_key_count,
            index_count: stats.index_count,
            memory_bytes: stats.memory_bytes,
            disk_bytes: stats.disk_bytes,
        })
    }

    /// Get WAL status for a database.
    pub async fn wal_status(
        databases: &DatabaseManager,
        db_name: &str,
    ) -> Result<types::WalStatusInfo, ServiceError> {
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        let status = tokio::task::spawn_blocking(move || entry.db.wal_status())
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
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        tokio::task::spawn_blocking(move || entry.db.wal_checkpoint())
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?
            .map_err(|e| ServiceError::Internal(e.to_string()))
    }

    /// Validate database integrity.
    pub async fn validate(
        databases: &DatabaseManager,
        db_name: &str,
    ) -> Result<types::ValidationInfo, ServiceError> {
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        let result = tokio::task::spawn_blocking(move || entry.db.validate())
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
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        tokio::task::spawn_blocking(move || match index {
            types::IndexDef::Property { property } => {
                entry.db.create_property_index(&property);
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
                .db
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
                .db
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
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        let stats = tokio::task::spawn_blocking(move || entry.db.query_cache().stats())
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
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        tokio::task::spawn_blocking(move || entry.db.clear_plan_cache())
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))
    }

    /// Get hierarchical memory usage breakdown for a database.
    pub async fn memory_usage(
        databases: &DatabaseManager,
        db_name: &str,
    ) -> Result<serde_json::Value, ServiceError> {
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        let usage = tokio::task::spawn_blocking(move || entry.db.memory_usage())
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;

        serde_json::to_value(&usage).map_err(|e| ServiceError::Internal(e.to_string()))
    }

    /// List named graphs within a database.
    pub async fn list_graphs(
        databases: &DatabaseManager,
        db_name: &str,
    ) -> Result<Vec<String>, ServiceError> {
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        tokio::task::spawn_blocking(move || entry.db.list_graphs())
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
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        tokio::task::spawn_blocking(move || entry.db.create_graph(&graph_name))
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
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        tokio::task::spawn_blocking(move || entry.db.drop_graph(&graph_name))
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))
    }

    /// Drop an index from a database.
    pub async fn drop_index(
        databases: &DatabaseManager,
        db_name: &str,
        index: types::IndexDef,
    ) -> Result<bool, ServiceError> {
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        tokio::task::spawn_blocking(move || match index {
            types::IndexDef::Property { property } => entry.db.drop_property_index(&property),
            #[cfg(feature = "vector-index")]
            types::IndexDef::Vector {
                label, property, ..
            } => entry.db.drop_vector_index(&label, &property),
            #[cfg(not(feature = "vector-index"))]
            types::IndexDef::Vector { .. } => false,
            #[cfg(feature = "text-index")]
            types::IndexDef::Text { label, property } => {
                entry.db.drop_text_index(&label, &property)
            }
            #[cfg(not(feature = "text-index"))]
            types::IndexDef::Text { .. } => false,
        })
        .await
        .map_err(|e| ServiceError::Internal(e.to_string()))
    }
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
}
