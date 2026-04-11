//! Database management endpoints.

use axum::extract::{Json, Path, State};
use axum::response::IntoResponse;

use crate::error::{ApiError, ErrorBody};
use crate::state::AppState;
use crate::types::{
    CreateDatabaseRequest, DatabaseInfoResponse, DatabaseSchemaResponse, DatabaseStatsResponse,
    DatabaseSummary, EdgeTypeInfo, LabelInfo, ListDatabasesResponse,
};

use grafeo_service::admin::AdminService;

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

    let db = entry.db();
    Ok(Json(DatabaseSummary {
        name,
        node_count: db.node_count(),
        edge_count: db.edge_count(),
        persistent: db.path().is_some(),
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
    let entry = state.databases().get_available(&name)?;

    let db = entry.db();
    let info = db.info();
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
        memory_limit_bytes: db.memory_limit(),
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
    let entry = state.databases().get_available(&name)?;

    let stats = entry.db().detailed_stats();
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
    let entry = state.databases().get_available(&name)?;

    let schema = entry.db().schema();
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
        grafeo_engine::admin::SchemaInfo::Rdf(_) | _ => Err(ApiError::bad_request(
            "RDF schema not supported via this endpoint",
        )),
    }
}

// ---------------------------------------------------------------------------
// Named graph endpoints (sub-graphs within a database)
// ---------------------------------------------------------------------------

/// List named graphs within a database.
///
/// Returns all named sub-graphs within the specified database.
/// Named graphs are distinct from databases: they are sub-graphs
/// within a single database instance.
#[utoipa::path(
    get,
    path = "/db/{name}/graphs",
    params(
        ("name" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "List of named graphs", body = grafeo_service::types::GraphListResponse),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Database"
)]
pub async fn list_graphs(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<grafeo_service::types::GraphListResponse>, ApiError> {
    let graphs = AdminService::list_graphs(state.databases(), &name).await?;
    Ok(Json(grafeo_service::types::GraphListResponse { graphs }))
}

/// Create a named graph within a database.
///
/// Creates a new sub-graph within the specified database.
/// Returns whether the graph was newly created (false if it already existed).
#[utoipa::path(
    post,
    path = "/db/{name}/graphs",
    params(
        ("name" = String, Path, description = "Database name"),
    ),
    request_body = grafeo_service::types::CreateGraphRequest,
    responses(
        (status = 200, description = "Graph created"),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Database"
)]
pub async fn create_graph(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<grafeo_service::types::CreateGraphRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let created = AdminService::create_graph(state.databases(), &name, req.name).await?;
    Ok(Json(serde_json::json!({ "created": created })))
}

/// Drop a named graph from a database.
///
/// Removes a sub-graph from the specified database.
/// Returns whether the graph existed and was removed.
#[utoipa::path(
    delete,
    path = "/db/{name}/graphs/{graph}",
    params(
        ("name" = String, Path, description = "Database name"),
        ("graph" = String, Path, description = "Graph name to drop"),
    ),
    responses(
        (status = 200, description = "Graph drop result"),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Database"
)]
pub async fn drop_graph(
    State(state): State<AppState>,
    Path((name, graph)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let dropped = AdminService::drop_graph(state.databases(), &name, graph).await?;
    Ok(Json(serde_json::json!({ "dropped": dropped })))
}

// ---------------------------------------------------------------------------
// Bulk import endpoints
// ---------------------------------------------------------------------------

/// Bulk-import a TSV edge list into a database.
///
/// Imports a tab or space-separated edge list. Each line should contain
/// `src_id dst_id`, with optional comment lines starting with `#` or `%`.
///
/// This bypasses per-edge transaction overhead, achieving 10-100x
/// throughput over individual inserts for large graphs.
#[utoipa::path(
    post,
    path = "/db/{name}/import/tsv",
    params(
        ("name" = String, Path, description = "Database name"),
    ),
    request_body = grafeo_service::types::ImportTsvRequest,
    responses(
        (status = 200, description = "Import result", body = grafeo_service::types::ImportResponse),
        (status = 400, description = "Malformed data", body = ErrorBody),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Database"
)]
pub async fn import_tsv(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<grafeo_service::types::ImportTsvRequest>,
) -> Result<Json<grafeo_service::types::ImportResponse>, ApiError> {
    let result = AdminService::import_tsv(
        state.databases(),
        &name,
        req.data,
        req.edge_type,
        req.directed,
    )
    .await?;
    Ok(Json(result))
}

// ---------------------------------------------------------------------------
// Schema namespace endpoints (ISO/IEC 39075 Section 4.2.5)
// ---------------------------------------------------------------------------

/// List schema namespaces within a database.
///
/// Returns all GQL schema namespaces. Schemas provide data isolation
/// within a single database instance.
#[utoipa::path(
    get,
    path = "/db/{name}/schemas",
    params(
        ("name" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "Schema list", body = grafeo_service::types::SchemaListResponse),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Database"
)]
pub async fn list_schemas(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<grafeo_service::types::SchemaListResponse>, ApiError> {
    let schemas = AdminService::list_schemas(state.databases(), state.metrics(), &name).await?;
    Ok(Json(grafeo_service::types::SchemaListResponse { schemas }))
}

/// Create a schema namespace within a database.
///
/// Creates a new GQL schema namespace with its own default graph.
/// Data within a schema is fully isolated from other schemas.
#[utoipa::path(
    post,
    path = "/db/{name}/schemas",
    params(
        ("name" = String, Path, description = "Database name"),
    ),
    request_body = grafeo_service::types::CreateSchemaRequest,
    responses(
        (status = 200, description = "Schema created", body = inline(serde_json::Value)),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Database"
)]
pub async fn create_schema(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<grafeo_service::types::CreateSchemaRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let created =
        AdminService::create_schema(state.databases(), state.metrics(), &name, &req.name).await?;
    Ok(Json(serde_json::json!({ "created": created })))
}

/// Drop a schema namespace from a database.
///
/// Removes a GQL schema namespace and all data within it.
/// Returns whether the schema existed and was removed.
#[utoipa::path(
    delete,
    path = "/db/{name}/schemas/{schema}",
    params(
        ("name" = String, Path, description = "Database name"),
        ("schema" = String, Path, description = "Schema namespace to drop"),
    ),
    responses(
        (status = 200, description = "Schema drop result", body = inline(serde_json::Value)),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Database"
)]
pub async fn drop_schema(
    State(state): State<AppState>,
    Path((name, schema)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let dropped =
        AdminService::drop_schema(state.databases(), state.metrics(), &name, &schema).await?;
    Ok(Json(serde_json::json!({ "dropped": dropped })))
}
