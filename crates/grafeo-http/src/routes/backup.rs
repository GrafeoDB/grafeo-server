//! Backup and restore endpoints.

use axum::body::Body;
use axum::extract::{Json, Path, State};
use axum::http::header;
use axum::response::IntoResponse;

use crate::error::ApiError;
use crate::middleware::auth_context::AuthContext;
use crate::state::AppState;

use grafeo_service::backup::BackupService;
use grafeo_service::types;

/// Create a full backup of a database.
///
/// Exports a point-in-time snapshot. The database stays available during
/// the backup (hot snapshot via MVCC checkpoint).
#[utoipa::path(
    post,
    path = "/admin/{db}/backup",
    params(
        ("db" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "Backup created", body = types::BackupEntry),
        (status = 400, description = "Backup not configured", body = crate::error::ErrorBody),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn create_backup(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
) -> Result<Json<types::BackupEntry>, ApiError> {
    auth.check_admin()?;
    let backup_dir = require_backup_dir(&state)?;

    let entry = BackupService::backup_database(state.databases(), &db, &backup_dir).await?;

    if let Some(keep) = state.backup_retention() {
        let _ = BackupService::enforce_retention(&db, &backup_dir, keep);
    }

    Ok(Json(entry))
}

/// List backups for a specific database.
#[utoipa::path(
    get,
    path = "/admin/{db}/backups",
    params(
        ("db" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "List of backups", body = Vec<types::BackupEntry>),
        (status = 400, description = "Backup not configured", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn list_backups(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
) -> Result<Json<Vec<types::BackupEntry>>, ApiError> {
    auth.check_admin()?;
    let backup_dir = require_backup_dir(&state)?;
    let entries = BackupService::list_backups(Some(&db), &backup_dir)?;
    Ok(Json(entries))
}

/// List all backups across all databases.
#[utoipa::path(
    get,
    path = "/backups",
    responses(
        (status = 200, description = "List of all backups", body = Vec<types::BackupEntry>),
        (status = 400, description = "Backup not configured", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn list_all_backups(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<Json<Vec<types::BackupEntry>>, ApiError> {
    auth.check_admin()?;
    let backup_dir = require_backup_dir(&state)?;
    let entries = BackupService::list_backups(None, &backup_dir)?;
    Ok(Json(entries))
}

/// Restore a database from a backup file.
///
/// Creates a safety backup before replacing data. The database returns 503
/// during the restore.
#[utoipa::path(
    post,
    path = "/admin/{db}/restore",
    params(
        ("db" = String, Path, description = "Database name"),
    ),
    request_body = types::RestoreRequest,
    responses(
        (status = 200, description = "Database restored"),
        (status = 400, description = "Bad request", body = crate::error::ErrorBody),
        (status = 403, description = "Server is read-only", body = crate::error::ErrorBody),
        (status = 404, description = "Database or backup not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn restore_backup(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
    Json(req): Json<types::RestoreRequest>,
) -> Result<impl IntoResponse, ApiError> {
    auth.check_admin()?;
    let backup_dir = require_backup_dir(&state)?;

    // Validate both params — axum percent-decodes path segments
    for param in [&db, &req.backup] {
        if param.contains('/') || param.contains('\\') || param.contains("..") {
            return Err(grafeo_service::error::ServiceError::BadRequest(
                "invalid path parameter".to_string(),
            )
            .into());
        }
    }

    let backup_path = backup_dir.join(&db).join(&req.backup);

    BackupService::restore_database(state.databases(), &db, &backup_path, &backup_dir).await?;

    Ok(Json(serde_json::json!({ "restored": true })))
}

/// Delete a specific backup file from a database.
#[utoipa::path(
    delete,
    path = "/admin/{db}/backups/{filename}",
    params(
        ("db" = String, Path, description = "Database name"),
        ("filename" = String, Path, description = "Backup filename"),
    ),
    responses(
        (status = 200, description = "Backup deleted"),
        (status = 400, description = "Bad request", body = crate::error::ErrorBody),
        (status = 404, description = "Backup not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn delete_backup(
    State(state): State<AppState>,
    auth: AuthContext,
    Path((db, filename)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    auth.check_admin()?;
    let backup_dir = require_backup_dir(&state)?;
    BackupService::delete_backup(&db, &filename, &backup_dir)?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

/// Download a backup file.
///
/// Streams the backup file with `Content-Disposition: attachment`.
#[utoipa::path(
    get,
    path = "/admin/{db}/backups/download/{filename}",
    params(
        ("db" = String, Path, description = "Database name"),
        ("filename" = String, Path, description = "Backup filename"),
    ),
    responses(
        (status = 200, description = "Backup file download"),
        (status = 400, description = "Bad request", body = crate::error::ErrorBody),
        (status = 404, description = "Backup not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn download_backup(
    State(state): State<AppState>,
    auth: AuthContext,
    Path((db, filename)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    auth.check_admin()?;
    let backup_dir = require_backup_dir(&state)?;

    // Validate both params — axum percent-decodes path segments
    for param in [&db, &filename] {
        if param.contains('/') || param.contains('\\') || param.contains("..") {
            return Err(grafeo_service::error::ServiceError::BadRequest(
                "invalid path parameter".to_string(),
            )
            .into());
        }
    }

    grafeo_service::backup::ensure_migrated(&backup_dir);

    let file_path = backup_dir.join(&db).join(&filename);
    if !file_path.exists() {
        return Err(grafeo_service::error::ServiceError::NotFound(format!(
            "backup '{filename}' not found for database '{db}'"
        ))
        .into());
    }

    let file = tokio::fs::File::open(&file_path)
        .await
        .map_err(|e| grafeo_service::error::ServiceError::Internal(e.to_string()))?;

    let stream = tokio_util::io::ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let safe_filename: String = filename
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();

    let headers = [
        (header::CONTENT_TYPE, "application/octet-stream".to_string()),
        (
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{safe_filename}\""),
        ),
    ];

    Ok((headers, body))
}

fn require_backup_dir(state: &AppState) -> Result<std::path::PathBuf, ApiError> {
    state
        .backup_dir()
        .ok_or_else(|| {
            grafeo_service::error::ServiceError::BadRequest(
                "backup not configured: start server with --backup-dir".to_string(),
            )
        })
        .map(|p| p.to_path_buf())
        .map_err(Into::into)
}
