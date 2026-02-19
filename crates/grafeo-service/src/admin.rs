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
}
