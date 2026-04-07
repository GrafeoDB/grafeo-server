//! Admin endpoints — database introspection, maintenance, and index management.

use axum::extract::{Json, Path, State};
use axum::response::IntoResponse;

use crate::error::ApiError;
use crate::state::AppState;

use grafeo_service::admin::AdminService;
use grafeo_service::types;

/// Get detailed database statistics.
///
/// Returns node/edge counts, label counts, memory and disk usage.
#[utoipa::path(
    get,
    path = "/admin/{db}/stats",
    params(
        ("db" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "Database statistics", body = types::DatabaseStats),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_stats(
    State(state): State<AppState>,
    Path(db): Path<String>,
) -> Result<Json<types::DatabaseStats>, ApiError> {
    let stats = AdminService::database_stats(state.databases(), &db).await?;
    Ok(Json(stats))
}

/// Get WAL status for a database.
///
/// Returns WAL enabled state, size, record count, and checkpoint info.
#[utoipa::path(
    get,
    path = "/admin/{db}/wal",
    params(
        ("db" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "WAL status", body = types::WalStatusInfo),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_wal_status(
    State(state): State<AppState>,
    Path(db): Path<String>,
) -> Result<Json<types::WalStatusInfo>, ApiError> {
    let status = AdminService::wal_status(state.databases(), &db).await?;
    Ok(Json(status))
}

/// Force a WAL checkpoint.
///
/// Flushes all pending WAL records to the main storage.
#[utoipa::path(
    post,
    path = "/admin/{db}/wal/checkpoint",
    params(
        ("db" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "Checkpoint completed"),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_wal_checkpoint(
    State(state): State<AppState>,
    Path(db): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    AdminService::wal_checkpoint(state.databases(), &db).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// Validate database integrity.
///
/// Checks for dangling edge references and internal consistency.
#[utoipa::path(
    get,
    path = "/admin/{db}/validate",
    params(
        ("db" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "Validation result", body = types::ValidationInfo),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_validate(
    State(state): State<AppState>,
    Path(db): Path<String>,
) -> Result<Json<types::ValidationInfo>, ApiError> {
    let result = AdminService::validate(state.databases(), &db).await?;
    Ok(Json(result))
}

/// Create an index on a database.
///
/// Supports property (hash), vector (HNSW), and text (BM25) indexes.
#[utoipa::path(
    post,
    path = "/admin/{db}/index",
    params(
        ("db" = String, Path, description = "Database name"),
    ),
    request_body = types::IndexDef,
    responses(
        (status = 200, description = "Index created"),
        (status = 400, description = "Invalid index definition", body = crate::error::ErrorBody),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_create_index(
    State(state): State<AppState>,
    Path(db): Path<String>,
    Json(index): Json<types::IndexDef>,
) -> Result<impl IntoResponse, ApiError> {
    AdminService::create_index(state.databases(), &db, index).await?;
    Ok(Json(serde_json::json!({ "created": true })))
}

/// Get query plan cache statistics.
///
/// Returns hit/miss counts, cache sizes, and invalidation count.
#[utoipa::path(
    get,
    path = "/admin/{db}/cache",
    params(
        ("db" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "Cache statistics", body = types::CacheStatsInfo),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_cache_stats(
    State(state): State<AppState>,
    Path(db): Path<String>,
) -> Result<Json<types::CacheStatsInfo>, ApiError> {
    let stats = AdminService::cache_stats(state.databases(), &db).await?;
    Ok(Json(stats))
}

/// Clear the query plan cache.
///
/// Forces re-parsing and re-optimization of all queries.
#[utoipa::path(
    post,
    path = "/admin/{db}/cache/clear",
    params(
        ("db" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "Cache cleared"),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_clear_cache(
    State(state): State<AppState>,
    Path(db): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    AdminService::clear_cache(state.databases(), &db).await?;
    Ok(Json(serde_json::json!({ "cleared": true })))
}

/// Get hierarchical memory usage breakdown.
///
/// Returns a detailed breakdown of memory usage across store, indexes,
/// MVCC, caches, string pools, and buffer manager.
#[utoipa::path(
    get,
    path = "/admin/{db}/memory",
    params(
        ("db" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "Memory usage breakdown"),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_memory_usage(
    State(state): State<AppState>,
    Path(db): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let usage = AdminService::memory_usage(state.databases(), &db).await?;
    Ok(Json(usage))
}

/// Drop an index from a database.
///
/// Returns whether the index existed and was removed.
#[utoipa::path(
    delete,
    path = "/admin/{db}/index",
    params(
        ("db" = String, Path, description = "Database name"),
    ),
    request_body = types::IndexDef,
    responses(
        (status = 200, description = "Index drop result"),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_drop_index(
    State(state): State<AppState>,
    Path(db): Path<String>,
    Json(index): Json<types::IndexDef>,
) -> Result<impl IntoResponse, ApiError> {
    let dropped = AdminService::drop_index(state.databases(), &db, index).await?;
    Ok(Json(serde_json::json!({ "dropped": dropped })))
}

/// Compact a database into a columnar read-only store.
///
/// Converts the live LPG store into a memory-efficient columnar format
/// optimized for analytical reads. This is a **one-way operation**: the
/// database becomes permanently read-only after compaction.
///
/// Requires the `compact-store` feature and exclusive access (no active
/// sessions).
#[utoipa::path(
    post,
    path = "/admin/{db}/compact",
    params(
        ("db" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "Database compacted"),
        (status = 400, description = "Feature not enabled", body = crate::error::ErrorBody),
        (status = 403, description = "Server is read-only", body = crate::error::ErrorBody),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
        (status = 409, description = "Database in use", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_compact(
    State(state): State<AppState>,
    Path(db): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    AdminService::compact(state.databases(), &db).await?;
    Ok(Json(serde_json::json!({ "compacted": true })))
}

/// Write a point-in-time snapshot.
///
/// Creates a `.grafeo` single-file snapshot of the database. Requires the
/// `async-storage` and `grafeo-file` features to be enabled.
#[utoipa::path(
    post,
    path = "/admin/{db}/snapshot",
    params(
        ("db" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "Snapshot created"),
        (status = 400, description = "Feature not enabled", body = crate::error::ErrorBody),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_write_snapshot(
    State(state): State<AppState>,
    Path(db): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    AdminService::write_snapshot(state.databases(), &db).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}
