//! Backup and restore endpoints.

use axum::body::Body;
use axum::extract::{Json, Path, State};
use axum::http::header;
use axum::response::IntoResponse;

use crate::error::ApiError;
use crate::state::AppState;

use grafeo_service::backup::BackupService;
use grafeo_service::types;

/// Create a backup of a database.
///
/// Exports a point-in-time snapshot to a `.grafeo` file. The database
/// remains fully available during the backup (hot snapshot via MVCC).
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
    Path(db): Path<String>,
) -> Result<Json<types::BackupEntry>, ApiError> {
    let backup_dir = state
        .backup_dir()
        .ok_or_else(|| {
            grafeo_service::error::ServiceError::BadRequest(
                "backup not configured: start server with --backup-dir".to_string(),
            )
        })?
        .to_path_buf();

    let entry = BackupService::backup_database(state.databases(), &db, &backup_dir).await?;

    // Enforce retention if configured
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
    Path(db): Path<String>,
) -> Result<Json<Vec<types::BackupEntry>>, ApiError> {
    let backup_dir = state
        .backup_dir()
        .ok_or_else(|| {
            grafeo_service::error::ServiceError::BadRequest(
                "backup not configured: start server with --backup-dir".to_string(),
            )
        })?
        .to_path_buf();

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
) -> Result<Json<Vec<types::BackupEntry>>, ApiError> {
    let backup_dir = state
        .backup_dir()
        .ok_or_else(|| {
            grafeo_service::error::ServiceError::BadRequest(
                "backup not configured: start server with --backup-dir".to_string(),
            )
        })?
        .to_path_buf();

    let entries = BackupService::list_backups(None, &backup_dir)?;
    Ok(Json(entries))
}

/// Restore a database from a backup file.
///
/// Creates a safety backup before replacing data. The database is briefly
/// unavailable during the restore (close, replace, reopen).
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
    Path(db): Path<String>,
    Json(req): Json<types::RestoreRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let backup_dir = state
        .backup_dir()
        .ok_or_else(|| {
            grafeo_service::error::ServiceError::BadRequest(
                "backup not configured: start server with --backup-dir".to_string(),
            )
        })?
        .to_path_buf();

    // Prevent path traversal in backup filename
    if req.backup.contains('/') || req.backup.contains('\\') || req.backup.contains("..") {
        return Err(grafeo_service::error::ServiceError::BadRequest(
            "invalid backup filename".to_string(),
        )
        .into());
    }

    let backup_path = backup_dir.join(&req.backup);

    BackupService::restore_database(state.databases(), &db, &backup_path, &backup_dir).await?;

    Ok(Json(serde_json::json!({ "restored": true })))
}

/// Delete a specific backup file.
#[utoipa::path(
    delete,
    path = "/backups/{filename}",
    params(
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
    Path(filename): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let backup_dir = state
        .backup_dir()
        .ok_or_else(|| {
            grafeo_service::error::ServiceError::BadRequest(
                "backup not configured: start server with --backup-dir".to_string(),
            )
        })?
        .to_path_buf();

    // Path traversal check (defense-in-depth; service layer also validates)
    if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
        return Err(grafeo_service::error::ServiceError::BadRequest(
            "invalid backup filename".to_string(),
        )
        .into());
    }

    BackupService::delete_backup(&filename, &backup_dir)?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

/// Download a backup file.
///
/// Streams the `.grafeo` file with `Content-Disposition: attachment`.
#[utoipa::path(
    get,
    path = "/admin/backups/download/{filename}",
    params(
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
    Path(filename): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let backup_dir = state
        .backup_dir()
        .ok_or_else(|| {
            grafeo_service::error::ServiceError::BadRequest(
                "backup not configured: start server with --backup-dir".to_string(),
            )
        })?
        .to_path_buf();

    // Prevent path traversal
    if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
        return Err(grafeo_service::error::ServiceError::BadRequest(
            "invalid backup filename".to_string(),
        )
        .into());
    }

    let path = backup_dir.join(&filename);
    if !path.exists() {
        return Err(grafeo_service::error::ServiceError::NotFound(format!(
            "backup '{filename}' not found"
        ))
        .into());
    }

    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| grafeo_service::error::ServiceError::Internal(e.to_string()))?;

    let stream = tokio_util::io::ReaderStream::new(file);
    let body = Body::from_stream(stream);

    // Sanitize filename for Content-Disposition: strip characters that
    // could break or manipulate the header value.
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
