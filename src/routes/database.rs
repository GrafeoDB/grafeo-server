//! Database management endpoints.

use axum::extract::{Json, Path, State};
use axum::response::IntoResponse;

use crate::error::{ApiError, ErrorBody};
use crate::state::AppState;

use super::types::{
    CreateDatabaseRequest, DatabaseInfoResponse, DatabaseSchemaResponse, DatabaseStatsResponse,
    DatabaseSummary, EdgeTypeInfo, LabelInfo, ListDatabasesResponse,
};

/// List all databases.
///
/// Returns summary information for each database including node/edge counts.
#[utoipa::path(
    get,
    path = "/db",
    responses(
        (status = 200, description = "List of databases", body = ListDatabasesResponse),
    ),
    tag = "Database"
)]
pub async fn list_databases(State(state): State<AppState>) -> impl IntoResponse {
    let databases = state.databases().list();
    Json(ListDatabasesResponse { databases })
}

/// Create a new database.
///
/// Creates a named database with optional type, storage mode, and resource settings.
/// Name must start with a letter, contain only alphanumeric characters, underscores,
/// or hyphens, and be at most 64 characters.
#[utoipa::path(
    post,
    path = "/db",
    request_body = CreateDatabaseRequest,
    responses(
        (status = 200, description = "Database created", body = DatabaseSummary),
        (status = 400, description = "Invalid request", body = ErrorBody),
        (status = 409, description = "Database already exists", body = ErrorBody),
    ),
    tag = "Database"
)]
pub async fn create_database(
    State(state): State<AppState>,
    Json(req): Json<CreateDatabaseRequest>,
) -> Result<Json<DatabaseSummary>, ApiError> {
    let name = req.name.clone();
    let db_type = req.database_type;
    state.databases().create(&req)?;

    let entry = state
        .databases()
        .get(&name)
        .ok_or_else(|| ApiError::internal("database disappeared after creation"))?;

    Ok(Json(DatabaseSummary {
        name,
        node_count: entry.db.node_count(),
        edge_count: entry.db.edge_count(),
        persistent: entry.db.path().is_some(),
        database_type: db_type.as_str().to_string(),
    }))
}

/// Delete a database.
///
/// Removes a named database and all its data. The "default" database cannot be deleted.
#[utoipa::path(
    delete,
    path = "/db/{name}",
    params(
        ("name" = String, Path, description = "Database name to delete"),
    ),
    responses(
        (status = 200, description = "Database deleted"),
        (status = 400, description = "Cannot delete default database", body = ErrorBody),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Database"
)]
pub async fn delete_database(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    // Clean up any transaction sessions belonging to this database
    state.sessions().remove_by_database(&name);
    state.databases().delete(&name)?;
    Ok(Json(serde_json::json!({ "deleted": name })))
}

/// Get database info.
///
/// Returns metadata about a specific database.
#[utoipa::path(
    get,
    path = "/db/{name}",
    params(
        ("name" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "Database info", body = DatabaseInfoResponse),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Database"
)]
pub async fn database_info(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<DatabaseInfoResponse>, ApiError> {
    let entry = state
        .databases()
        .get(&name)
        .ok_or_else(|| ApiError::not_found(format!("database '{name}' not found")))?;

    let info = entry.db.info();
    let metadata = &entry.metadata;
    Ok(Json(DatabaseInfoResponse {
        name,
        node_count: info.node_count,
        edge_count: info.edge_count,
        persistent: info.is_persistent,
        version: info.version,
        wal_enabled: info.wal_enabled,
        database_type: metadata.database_type.clone(),
        storage_mode: metadata.storage_mode.clone(),
        memory_limit_bytes: entry.db.memory_limit(),
        backward_edges: metadata.backward_edges,
        threads: metadata.threads,
    }))
}

/// Get database statistics.
///
/// Returns detailed statistics including memory and disk usage.
#[utoipa::path(
    get,
    path = "/db/{name}/stats",
    params(
        ("name" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "Database statistics", body = DatabaseStatsResponse),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Database"
)]
pub async fn database_stats(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<DatabaseStatsResponse>, ApiError> {
    let entry = state
        .databases()
        .get(&name)
        .ok_or_else(|| ApiError::not_found(format!("database '{name}' not found")))?;

    let stats = entry.db.detailed_stats();
    Ok(Json(DatabaseStatsResponse {
        name,
        node_count: stats.node_count,
        edge_count: stats.edge_count,
        label_count: stats.label_count,
        edge_type_count: stats.edge_type_count,
        property_key_count: stats.property_key_count,
        index_count: stats.index_count,
        memory_bytes: stats.memory_bytes,
        disk_bytes: stats.disk_bytes,
    }))
}

/// Get database schema.
///
/// Returns labels, edge types, and property keys for a database.
#[utoipa::path(
    get,
    path = "/db/{name}/schema",
    params(
        ("name" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "Database schema", body = DatabaseSchemaResponse),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Database"
)]
pub async fn database_schema(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<DatabaseSchemaResponse>, ApiError> {
    let entry = state
        .databases()
        .get(&name)
        .ok_or_else(|| ApiError::not_found(format!("database '{name}' not found")))?;

    let schema = entry.db.schema();
    match schema {
        grafeo_engine::admin::SchemaInfo::Lpg(lpg) => Ok(Json(DatabaseSchemaResponse {
            name,
            labels: lpg
                .labels
                .into_iter()
                .map(|l| LabelInfo {
                    name: l.name,
                    count: l.count,
                })
                .collect(),
            edge_types: lpg
                .edge_types
                .into_iter()
                .map(|e| EdgeTypeInfo {
                    name: e.name,
                    count: e.count,
                })
                .collect(),
            property_keys: lpg.property_keys,
        })),
        grafeo_engine::admin::SchemaInfo::Rdf(_) => Err(ApiError::bad_request(
            "RDF schema not supported via this endpoint",
        )),
    }
}
