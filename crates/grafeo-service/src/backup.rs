//! Backup and restore operations for managed databases.
//!
//! Uses the engine's `save()` for hot snapshots (MVCC means reads/writes
//! continue during export) and the `close()`/`open()` lifecycle for restore.

use std::path::Path;
use std::sync::Arc;

use grafeo_engine::GrafeoDB;

use crate::database::{DatabaseEntry, DatabaseManager};
use crate::error::ServiceError;
use crate::types;

/// Stateless backup and restore operations.
pub struct BackupService;

impl BackupService {
    /// Create a backup of a database.
    ///
    /// Exports a point-in-time snapshot to a `.grafeo` file in `backup_dir`.
    /// The database remains fully available during the backup (hot snapshot).
    pub async fn backup_database(
        databases: &DatabaseManager,
        db_name: &str,
        backup_dir: &Path,
    ) -> Result<types::BackupEntry, ServiceError> {
        let entry = databases.get_available(db_name)?;

        let timestamp = now_timestamp();
        let filename = format!("{db_name}_{timestamp}.grafeo");
        let backup_path = backup_dir.join(&filename);
        let db_name_owned = db_name.to_owned();

        // Ensure backup directory exists
        std::fs::create_dir_all(backup_dir).map_err(|e| {
            ServiceError::Internal(format!("failed to create backup directory: {e}"))
        })?;

        let db = entry.db();
        let node_count = db.node_count() as u64;
        let edge_count = db.edge_count() as u64;

        let path = backup_path.clone();
        tokio::task::spawn_blocking(move || db.save(&path))
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?
            .map_err(|e| ServiceError::Internal(format!("backup failed: {e}")))?;

        let size_bytes = std::fs::metadata(&backup_path)
            .map(|m| m.len())
            .unwrap_or(0);

        tracing::info!(
            database = %db_name_owned,
            filename = %filename,
            size_bytes,
            "Backup created"
        );

        Ok(types::BackupEntry {
            filename,
            database: db_name_owned,
            size_bytes,
            created_at: timestamp_to_iso(&timestamp),
            node_count,
            edge_count,
            epoch: 0, // TODO: expose epoch from engine if needed
        })
    }

    /// Restore a database from a backup file.
    ///
    /// The entry stays in the DashMap the entire time. Flow:
    /// 1. Mark entry as `Restoring` (incoming requests get 503)
    /// 2. Create safety backup via hot snapshot (DB still open)
    /// 3. Close the old handle
    /// 4. Replace data files on disk
    /// 5. Open a new handle from the backup data
    /// 6. Swap the new handle into the entry via ArcSwap
    /// 7. Mark entry as `Available`
    pub async fn restore_database(
        databases: &DatabaseManager,
        db_name: &str,
        backup_path: &Path,
        backup_dir: &Path,
    ) -> Result<(), ServiceError> {
        if databases.is_read_only() {
            return Err(ServiceError::ReadOnly);
        }

        // Verify backup exists
        if !backup_path.exists() {
            return Err(ServiceError::NotFound(format!(
                "backup file not found: {}",
                backup_path.display()
            )));
        }

        // Verify the database exists and is persistent
        let data_dir = databases.data_dir().ok_or_else(|| {
            ServiceError::BadRequest("restore requires persistent storage (--data-dir)".to_string())
        })?;

        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        // Must be persistent
        if entry.db().path().is_none() {
            return Err(ServiceError::BadRequest(
                "cannot restore an in-memory database".to_string(),
            ));
        }

        // Mark as restoring — incoming requests will get 503
        if !entry.set_restoring() {
            return Err(ServiceError::Conflict(
                "database is already being restored".to_string(),
            ));
        }

        // do_restore returns (result, recovered). If recovery swapped in a valid
        // handle, set available. If recovery failed completely, leave as Restoring
        // so callers get 503 instead of hitting a closed handle.
        let (result, has_valid_handle) =
            Self::do_restore(&entry, db_name, backup_path, backup_dir, data_dir).await;

        if has_valid_handle {
            entry.set_available();
        }
        result
    }

    /// Inner restore logic. Returns (result, has_valid_handle).
    /// `has_valid_handle` is true if the entry holds a usable database handle
    /// (either from successful restore or successful recovery).
    async fn do_restore(
        entry: &Arc<DatabaseEntry>,
        db_name: &str,
        backup_path: &Path,
        backup_dir: &Path,
        data_dir: &Path,
    ) -> (Result<(), ServiceError>, bool) {
        // 1. Safety backup (hot snapshot, DB still open and serving reads)
        tracing::info!(database = %db_name, "Creating safety backup before restore");
        let safety_timestamp = now_timestamp();
        let safety_filename = format!("{db_name}_{safety_timestamp}.grafeo");
        let safety_path = backup_dir.join(&safety_filename);

        if let Err(e) = std::fs::create_dir_all(backup_dir) {
            return (
                Err(ServiceError::Internal(format!(
                    "failed to create backup directory: {e}"
                ))),
                true,
            );
        }

        let safety_db = entry.db();
        let safety_path_clone = safety_path.clone();
        let save_result =
            tokio::task::spawn_blocking(move || safety_db.save(&safety_path_clone)).await;
        match save_result {
            Err(e) => return (Err(ServiceError::Internal(e.to_string())), true),
            Ok(Err(e)) => {
                return (
                    Err(ServiceError::Internal(format!("safety backup failed: {e}"))),
                    true,
                );
            }
            Ok(Ok(())) => {}
        }

        // 2. Close the old handle
        let old_db = entry.db();
        if let Err(e) = old_db.close() {
            tracing::warn!(database = %db_name, error = %e, "Error closing database for restore");
        }
        drop(old_db);

        // 3. Replace data files on disk
        let db_dir = data_dir.join(db_name);
        let db_file = db_dir.join("grafeo.db");

        if db_file.exists() {
            let remove_result = if db_file.is_dir() {
                std::fs::remove_dir_all(&db_file)
            } else {
                std::fs::remove_file(&db_file)
            };
            if let Err(e) = remove_result {
                return (
                    Err(ServiceError::Internal(format!(
                        "failed to remove old database: {e}"
                    ))),
                    true,
                );
            }
        }
        let wal_dir = db_dir.join("grafeo.db.wal");
        if wal_dir.exists()
            && let Err(e) = std::fs::remove_dir_all(&wal_dir)
        {
            return (
                Err(ServiceError::Internal(format!(
                    "failed to remove old WAL: {e}"
                ))),
                true,
            );
        }

        // 4. Open backup and save to the persistent path, then open the result
        let backup_owned = backup_path.to_path_buf();
        let db_file_owned = db_file.clone();
        let open_result = tokio::task::spawn_blocking(move || -> Result<GrafeoDB, String> {
            let backup_db = GrafeoDB::open(backup_owned.to_str().unwrap())
                .map_err(|e| format!("failed to open backup: {e}"))?;
            backup_db
                .save(&db_file_owned)
                .map_err(|e| format!("failed to save restored data: {e}"))?;
            backup_db.close().ok();
            GrafeoDB::open(db_file_owned.to_str().unwrap())
                .map_err(|e| format!("failed to open restored database: {e}"))
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r);

        match open_result {
            Ok(new_db) => {
                // 5. Swap the new handle in — lock-free, atomic
                entry.swap_db(Arc::new(new_db));
                tracing::info!(database = %db_name, "Database restored from backup");
                (Ok(()), true)
            }
            Err(e) => {
                // Recovery: reopen from safety backup via spawn_blocking
                tracing::error!(
                    database = %db_name,
                    error = %e,
                    "Failed to restore, recovering from safety backup"
                );
                let db_file_owned = db_file.clone();
                let safety_owned = safety_path.clone();
                let recovery_result = tokio::task::spawn_blocking(move || {
                    if db_file_owned.exists() {
                        if db_file_owned.is_dir() {
                            let _ = std::fs::remove_dir_all(&db_file_owned);
                        } else {
                            let _ = std::fs::remove_file(&db_file_owned);
                        }
                    }
                    if let Ok(safety_db) = GrafeoDB::open(safety_owned.to_str().unwrap()) {
                        let _ = safety_db.save(&db_file_owned);
                        safety_db.close().ok();
                    }
                    GrafeoDB::open(db_file_owned.to_str().unwrap())
                })
                .await;

                let recovered = match recovery_result {
                    Ok(Ok(db)) => {
                        entry.swap_db(Arc::new(db));
                        true
                    }
                    _ => false,
                };

                (
                    Err(ServiceError::Internal(format!(
                        "restore failed{}: {e}",
                        if recovered {
                            ", recovered from safety backup"
                        } else {
                            ", recovery also failed"
                        }
                    ))),
                    recovered,
                )
            }
        }
    }

    /// List available backups, optionally filtered by database name.
    pub fn list_backups(
        db_name: Option<&str>,
        backup_dir: &Path,
    ) -> Result<Vec<types::BackupEntry>, ServiceError> {
        if !backup_dir.exists() {
            return Ok(vec![]);
        }

        let entries = std::fs::read_dir(backup_dir)
            .map_err(|e| ServiceError::Internal(format!("failed to read backup directory: {e}")))?;

        let mut backups: Vec<types::BackupEntry> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let fname = path.file_name()?.to_str()?.to_string();
                if !fname.ends_with(".grafeo") {
                    return None;
                }

                // Parse database name and timestamp from filename
                // Format: {db_name}_{YYYY_MM_DD_HH_MM_SS}.grafeo
                let stem = fname.strip_suffix(".grafeo")?;
                let (db, timestamp) = parse_backup_filename(stem)?;

                // Filter by database name if specified
                if let Some(filter) = db_name
                    && db != filter
                {
                    return None;
                }

                let meta = std::fs::metadata(&path).ok()?;
                Some(types::BackupEntry {
                    filename: fname,
                    database: db,
                    size_bytes: meta.len(),
                    created_at: timestamp_to_iso(&timestamp),
                    node_count: 0,
                    edge_count: 0,
                    epoch: 0,
                })
            })
            .collect();

        // Sort newest first
        backups.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        Ok(backups)
    }

    /// Delete a specific backup file.
    pub fn delete_backup(filename: &str, backup_dir: &Path) -> Result<(), ServiceError> {
        // Prevent path traversal
        if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
            return Err(ServiceError::BadRequest(
                "invalid backup filename".to_string(),
            ));
        }

        let path = backup_dir.join(filename);
        if !path.exists() {
            return Err(ServiceError::NotFound(format!(
                "backup '{filename}' not found"
            )));
        }

        std::fs::remove_file(&path)
            .map_err(|e| ServiceError::Internal(format!("failed to delete backup: {e}")))?;

        tracing::info!(filename = %filename, "Backup deleted");
        Ok(())
    }

    /// Enforce retention policy: keep only the N most recent backups per database.
    /// Returns filenames of deleted backups.
    pub fn enforce_retention(
        db_name: &str,
        backup_dir: &Path,
        keep: usize,
    ) -> Result<Vec<String>, ServiceError> {
        let backups = Self::list_backups(Some(db_name), backup_dir)?;

        if backups.len() <= keep {
            return Ok(vec![]);
        }

        // backups is sorted newest-first, so skip `keep` and delete the rest
        let mut deleted = Vec::new();
        for backup in backups.into_iter().skip(keep) {
            let path = backup_dir.join(&backup.filename);
            if std::fs::remove_file(&path).is_ok() {
                tracing::info!(
                    database = %db_name,
                    filename = %backup.filename,
                    "Removed old backup (retention policy)"
                );
                deleted.push(backup.filename);
            }
        }

        Ok(deleted)
    }
}

/// Generate a timestamp string: YYYY_MM_DD_HH_MM_SS_mmm (UTC).
///
/// Includes milliseconds to avoid filename collisions when multiple
/// backups are created within the same second (e.g. safety backups
/// during back-to-back restores).
fn now_timestamp() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();

    // Manual UTC breakdown (no chrono dependency needed)
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Days since epoch to Y/M/D
    let (year, month, day) = days_to_ymd(days);

    format!("{year:04}_{month:02}_{day:02}_{hours:02}_{minutes:02}_{seconds:02}_{millis:03}")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Civil calendar algorithm from Howard Hinnant
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Convert our timestamp format to ISO 8601.
fn timestamp_to_iso(ts: &str) -> String {
    // "2024_01_15_02_30_45_123" -> "2024-01-15T02:30:45.123Z"
    let parts: Vec<&str> = ts.split('_').collect();
    if parts.len() == 7 {
        format!(
            "{}-{}-{}T{}:{}:{}.{}Z",
            parts[0], parts[1], parts[2], parts[3], parts[4], parts[5], parts[6]
        )
    } else if parts.len() == 6 {
        // Backwards compat with old timestamps without millis
        format!(
            "{}-{}-{}T{}:{}:{}Z",
            parts[0], parts[1], parts[2], parts[3], parts[4], parts[5]
        )
    } else {
        ts.to_string()
    }
}

/// Parse a backup filename stem into (database_name, timestamp).
/// Format: `{db_name}_{YYYY}_{MM}_{DD}_{HH}_{MM}_{SS}_{mmm}`
/// Also accepts the old 6-part format without milliseconds.
fn parse_backup_filename(stem: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = stem.split('_').collect();

    // Try 7-part timestamp first (with millis), then 6-part (without)
    let ts_len = if parts.len() >= 8
        && parts[parts.len() - 7..]
            .iter()
            .all(|p| p.chars().all(|c| c.is_ascii_digit()))
    {
        7
    } else if parts.len() >= 7
        && parts[parts.len() - 6..]
            .iter()
            .all(|p| p.chars().all(|c| c.is_ascii_digit()))
    {
        6
    } else {
        return None;
    };

    let ts_parts = &parts[parts.len() - ts_len..];
    let db_name = parts[..parts.len() - ts_len].join("_");
    let timestamp = ts_parts.join("_");

    if db_name.is_empty() {
        return None;
    }

    Some((db_name, timestamp))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_backup_filename_with_millis() {
        let (db, ts) = parse_backup_filename("default_2024_01_15_02_30_45_123").unwrap();
        assert_eq!(db, "default");
        assert_eq!(ts, "2024_01_15_02_30_45_123");
    }

    #[test]
    fn test_parse_backup_filename_without_millis() {
        let (db, ts) = parse_backup_filename("default_2024_01_15_02_30_45").unwrap();
        assert_eq!(db, "default");
        assert_eq!(ts, "2024_01_15_02_30_45");
    }

    #[test]
    fn test_parse_backup_filename_with_underscores() {
        let (db, ts) = parse_backup_filename("my_cool_db_2024_01_15_02_30_45_123").unwrap();
        assert_eq!(db, "my_cool_db");
        assert_eq!(ts, "2024_01_15_02_30_45_123");
    }

    #[test]
    fn test_parse_backup_filename_with_underscores_no_millis() {
        let (db, ts) = parse_backup_filename("my_cool_db_2024_01_15_02_30_45").unwrap();
        assert_eq!(db, "my_cool_db");
        assert_eq!(ts, "2024_01_15_02_30_45");
    }

    #[test]
    fn test_parse_backup_filename_multi_underscore_name() {
        let (db, ts) = parse_backup_filename("my_multi_part_name_2024_01_15_02_30_45_123").unwrap();
        assert_eq!(db, "my_multi_part_name");
        assert_eq!(ts, "2024_01_15_02_30_45_123");
    }

    #[test]
    fn test_parse_backup_filename_too_short() {
        assert!(parse_backup_filename("default_2024").is_none());
    }

    #[test]
    fn test_timestamp_to_iso_with_millis() {
        assert_eq!(
            timestamp_to_iso("2024_01_15_02_30_45_123"),
            "2024-01-15T02:30:45.123Z"
        );
    }

    #[test]
    fn test_timestamp_to_iso_without_millis() {
        assert_eq!(
            timestamp_to_iso("2024_01_15_02_30_45"),
            "2024-01-15T02:30:45Z"
        );
    }

    #[test]
    fn test_now_timestamp_format() {
        let ts = now_timestamp();
        let parts: Vec<&str> = ts.split('_').collect();
        assert_eq!(parts.len(), 7);
        for p in &parts {
            assert!(p.chars().all(|c| c.is_ascii_digit()));
        }
    }

    #[tokio::test]
    async fn test_backup_and_list() {
        let state = crate::ServiceState::new_in_memory(300);
        let backup_dir = tempfile::tempdir().unwrap();

        // Backup the default database
        let result =
            BackupService::backup_database(state.databases(), "default", backup_dir.path()).await;
        assert!(result.is_ok());

        let backup = result.unwrap();
        assert_eq!(backup.database, "default");
        assert!(backup.filename.starts_with("default_"));
        assert!(backup.filename.ends_with(".grafeo"));
        assert!(backup.size_bytes > 0);

        // List backups
        let list = BackupService::list_backups(None, backup_dir.path()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].database, "default");
    }

    #[tokio::test]
    async fn test_backup_not_found() {
        let state = crate::ServiceState::new_in_memory(300);
        let backup_dir = tempfile::tempdir().unwrap();
        let result =
            BackupService::backup_database(state.databases(), "nonexistent", backup_dir.path())
                .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_backups_empty() {
        let backup_dir = tempfile::tempdir().unwrap();
        let list = BackupService::list_backups(None, backup_dir.path()).unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn test_list_backups_nonexistent_dir() {
        let list = BackupService::list_backups(None, Path::new("/nonexistent/path")).unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn test_delete_backup() {
        let state = crate::ServiceState::new_in_memory(300);
        let backup_dir = tempfile::tempdir().unwrap();

        let backup =
            BackupService::backup_database(state.databases(), "default", backup_dir.path())
                .await
                .unwrap();

        // Delete it
        BackupService::delete_backup(&backup.filename, backup_dir.path()).unwrap();

        // Verify gone
        let list = BackupService::list_backups(None, backup_dir.path()).unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn test_delete_backup_path_traversal() {
        let backup_dir = tempfile::tempdir().unwrap();
        let result = BackupService::delete_backup("../etc/passwd", backup_dir.path());
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_enforce_retention() {
        let state = crate::ServiceState::new_in_memory(300);
        let backup_dir = tempfile::tempdir().unwrap();

        // Create 3 backups with slight delay so timestamps differ
        for _ in 0..3 {
            BackupService::backup_database(state.databases(), "default", backup_dir.path())
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        }

        let before = BackupService::list_backups(None, backup_dir.path()).unwrap();
        assert_eq!(before.len(), 3);

        // Keep only 1
        let deleted = BackupService::enforce_retention("default", backup_dir.path(), 1).unwrap();
        assert_eq!(deleted.len(), 2);

        let after = BackupService::list_backups(None, backup_dir.path()).unwrap();
        assert_eq!(after.len(), 1);
    }

    #[tokio::test]
    async fn test_enforce_retention_noop_when_under_limit() {
        let state = crate::ServiceState::new_in_memory(300);
        let backup_dir = tempfile::tempdir().unwrap();

        BackupService::backup_database(state.databases(), "default", backup_dir.path())
            .await
            .unwrap();

        let deleted = BackupService::enforce_retention("default", backup_dir.path(), 5).unwrap();
        assert!(deleted.is_empty());
    }

    #[tokio::test]
    async fn test_list_backups_filters_by_database() {
        let state = crate::ServiceState::new_in_memory(300);
        let backup_dir = tempfile::tempdir().unwrap();

        // Create a second database
        let req = crate::types::CreateDatabaseRequest {
            name: "other".to_string(),
            database_type: crate::types::DatabaseType::Lpg,
            storage_mode: crate::types::StorageMode::InMemory,
            options: crate::types::DatabaseOptions::default(),
            schema_file: None,
            schema_filename: None,
        };
        state.databases().create(&req).unwrap();

        // Backup both
        BackupService::backup_database(state.databases(), "default", backup_dir.path())
            .await
            .unwrap();
        BackupService::backup_database(state.databases(), "other", backup_dir.path())
            .await
            .unwrap();

        // All backups
        let all = BackupService::list_backups(None, backup_dir.path()).unwrap();
        assert_eq!(all.len(), 2);

        // Filter to default only
        let default_only = BackupService::list_backups(Some("default"), backup_dir.path()).unwrap();
        assert_eq!(default_only.len(), 1);
        assert_eq!(default_only[0].database, "default");

        // Filter to other only
        let other_only = BackupService::list_backups(Some("other"), backup_dir.path()).unwrap();
        assert_eq!(other_only.len(), 1);
        assert_eq!(other_only[0].database, "other");
    }

    #[tokio::test]
    async fn test_delete_nonexistent_backup() {
        let backup_dir = tempfile::tempdir().unwrap();
        let result = BackupService::delete_backup("does_not_exist.grafeo", backup_dir.path());
        assert!(matches!(result, Err(ServiceError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_backup_creates_directory() {
        let state = crate::ServiceState::new_in_memory(300);
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("deeply").join("nested").join("dir");

        let result = BackupService::backup_database(state.databases(), "default", &nested).await;
        assert!(result.is_ok());
        assert!(nested.exists());
    }

    #[tokio::test]
    async fn test_restore_requires_persistent_storage() {
        let state = crate::ServiceState::new_in_memory(300);
        let backup_dir = tempfile::tempdir().unwrap();

        // Create a backup
        let backup =
            BackupService::backup_database(state.databases(), "default", backup_dir.path())
                .await
                .unwrap();

        let backup_path = backup_dir.path().join(&backup.filename);

        // Restore should fail: in-memory mode has no data_dir
        let result = BackupService::restore_database(
            state.databases(),
            "default",
            &backup_path,
            backup_dir.path(),
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_restore_with_persistent_storage() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();

        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        // Insert data into the default database
        {
            let entry = mgr.get("default").unwrap();
            entry
                .db()
                .session()
                .execute("INSERT (:Person {name: 'Alice'})")
                .unwrap();
            assert_eq!(entry.db().node_count(), 1);
        }

        // Backup
        let backup = BackupService::backup_database(&mgr, "default", backup_dir.path())
            .await
            .unwrap();
        assert_eq!(backup.node_count, 1);

        // Insert more data after backup
        {
            let entry = mgr.get("default").unwrap();
            entry
                .db()
                .session()
                .execute("INSERT (:Person {name: 'Bob'})")
                .unwrap();
            assert_eq!(entry.db().node_count(), 2);
        }

        // Restore from backup (should go back to 1 node)
        let backup_path = backup_dir.path().join(&backup.filename);
        BackupService::restore_database(&mgr, "default", &backup_path, backup_dir.path())
            .await
            .unwrap();

        let entry = mgr.get("default").unwrap();
        assert_eq!(entry.db().node_count(), 1);
    }

    #[tokio::test]
    async fn test_restore_nonexistent_backup_file() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();

        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        let result = BackupService::restore_database(
            &mgr,
            "default",
            &backup_dir.path().join("nonexistent.grafeo"),
            backup_dir.path(),
        )
        .await;
        assert!(matches!(result, Err(ServiceError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_restore_nonexistent_database() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();

        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        // Create a valid backup file
        let backup = BackupService::backup_database(&mgr, "default", backup_dir.path())
            .await
            .unwrap();
        let backup_path = backup_dir.path().join(&backup.filename);

        let result =
            BackupService::restore_database(&mgr, "nonexistent", &backup_path, backup_dir.path())
                .await;
        assert!(matches!(result, Err(ServiceError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_restore_read_only_rejected() {
        // Use a read-only in-memory manager. The restore check for read_only
        // happens before any persistent storage checks.
        let mgr = crate::database::DatabaseManager::new(None, true);
        let backup_dir = tempfile::tempdir().unwrap();

        // Create a dummy backup file so the "file not found" check passes
        let dummy_path = backup_dir.path().join("dummy.grafeo");
        std::fs::write(&dummy_path, b"fake").unwrap();

        let result =
            BackupService::restore_database(&mgr, "default", &dummy_path, backup_dir.path()).await;
        assert!(matches!(result, Err(ServiceError::ReadOnly)));
    }

    #[test]
    fn test_timestamp_to_iso_invalid() {
        // Non-standard format passes through unchanged
        assert_eq!(timestamp_to_iso("not_a_timestamp"), "not_a_timestamp");
    }

    #[test]
    fn test_parse_backup_filename_non_numeric_timestamp() {
        assert!(parse_backup_filename("db_20xx_01_15_02_30_45").is_none());
    }

    #[test]
    fn test_days_to_ymd_epoch() {
        let (y, m, d) = days_to_ymd(0);
        assert_eq!((y, m, d), (1970, 1, 1));
    }

    #[test]
    fn test_days_to_ymd_known_date() {
        // 2024-01-15 = 19737 days since epoch
        let (y, m, d) = days_to_ymd(19737);
        assert_eq!((y, m, d), (2024, 1, 15));
    }
}
