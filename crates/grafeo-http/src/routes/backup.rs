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
/// the backup (hot snapshot via MVCC checkpoint). Accepts an optional
/// `{ label }` body — the label is stored in a sidecar file and surfaced
/// on listings; filenames remain engine-controlled.
#[utoipa::path(
    post,
    path = "/admin/{db}/backup",
    params(
        ("db" = String, Path, description = "Database name"),
    ),
    request_body = types::CreateBackupRequest,
    responses(
        (status = 200, description = "Backup created", body = types::BackupEntry),
        (status = 400, description = "Backup not configured or invalid label", body = crate::error::ErrorBody),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn create_backup(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
    body: Option<Json<types::CreateBackupRequest>>,
) -> Result<Json<types::BackupEntry>, ApiError> {
    auth.check_admin()?;
    let backup_dir = require_backup_dir(&state)?;

    let label = body.and_then(|Json(req)| req.label);
    let entry = BackupService::backup_database(state.databases(), &db, &backup_dir, label).await?;

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

    // Source database defaults to the target for same-db restores. When
    // the client provides source_db, this is a cross-database restore and
    // the backup file lives under {backup_dir}/{source_db}/ rather than
    // {backup_dir}/{target}/.
    let source_db = req.source_db.as_deref().unwrap_or(db.as_str());

    // Validate all three path components — axum percent-decodes URL
    // segments and the JSON body arrives as arbitrary strings, so both
    // paths need explicit traversal checks before they're joined.
    for param in [db.as_str(), source_db, req.backup.as_str()] {
        if param.contains('/') || param.contains('\\') || param.contains("..") {
            return Err(grafeo_service::error::ServiceError::BadRequest(
                "invalid path parameter".to_string(),
            )
            .into());
        }
    }

    let backup_path = backup_dir.join(source_db).join(&req.backup);

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

/// Create an incremental backup.
///
/// Exports WAL records since the last backup. Requires a persistent database
/// with at least one prior full backup.
#[utoipa::path(
    post,
    path = "/admin/{db}/backup/incremental",
    params(
        ("db" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "Incremental backup created", body = types::BackupEntry),
        (status = 400, description = "Not a persistent database", body = crate::error::ErrorBody),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn create_incremental_backup(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
) -> Result<Json<types::BackupEntry>, ApiError> {
    auth.check_admin()?;
    let backup_dir = require_backup_dir(&state)?;
    let entry = BackupService::backup_incremental(state.databases(), &db, &backup_dir).await?;
    Ok(Json(entry))
}

/// Restore a database to a specific epoch.
///
/// Replays the backup chain (full + incrementals) up to the target epoch,
/// then hot-swaps the database handle.
#[utoipa::path(
    post,
    path = "/admin/{db}/restore/epoch",
    params(
        ("db" = String, Path, description = "Database name"),
    ),
    request_body = types::RestoreToEpochRequest,
    responses(
        (status = 204, description = "Database restored to epoch"),
        (status = 400, description = "Invalid request", body = crate::error::ErrorBody),
        (status = 403, description = "Server is read-only", body = crate::error::ErrorBody),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn restore_to_epoch(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
    Json(req): Json<types::RestoreToEpochRequest>,
) -> Result<impl IntoResponse, ApiError> {
    auth.check_admin()?;
    let backup_dir = require_backup_dir(&state)?;
    BackupService::restore_to_epoch(state.databases(), &db, req.epoch, &backup_dir).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
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

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn app() -> axum::Router {
        let state = crate::AppState::new_in_memory(300);
        crate::router(state)
    }

    #[tokio::test]
    async fn incremental_backup_no_backup_dir() {
        // In-memory AppState has no backup dir configured
        let resp = app()
            .oneshot(
                Request::post("/admin/default/backup/incremental")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn restore_to_epoch_no_backup_dir() {
        let resp = app()
            .oneshot(
                Request::post("/admin/default/restore/epoch")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"epoch": 1}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn incremental_backup_database_not_found() {
        let resp = app()
            .oneshot(
                Request::post("/admin/nonexistent/backup/incremental")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // 400 because backup dir not configured (checked before DB lookup)
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
