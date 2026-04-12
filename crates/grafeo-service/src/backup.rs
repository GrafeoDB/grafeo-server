//! Backup and restore operations for managed databases.
//!
//! Uses the engine's backup chain API (`backup_full`) for hot snapshots with
//! real epoch tracking and checksums. Each database gets its own subdirectory
//! within the configured backup dir (`{backup_dir}/{db_name}/`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use grafeo_engine::database::backup::{BackupKind, BackupSegment};
use grafeo_engine::GrafeoDB;

use crate::database::{DatabaseEntry, DatabaseManager};
use crate::error::ServiceError;
use crate::types;

/// Returns the per-database backup subdirectory.
fn db_backup_dir(backup_dir: &Path, db_name: &str) -> Result<PathBuf, ServiceError> {
    if db_name.contains('/') || db_name.contains('\\') || db_name.contains("..") {
        return Err(ServiceError::BadRequest(
            "invalid database name".to_string(),
        ));
    }
    Ok(backup_dir.join(db_name))
}

/// Ensure legacy backups in the root backup directory are migrated to
/// per-database subdirectories. Safe to call multiple times.
pub fn ensure_migrated(backup_dir: &Path) {
    migrate_legacy_backups(backup_dir);
}

/// Stateless backup and restore operations.
pub struct BackupService;

impl BackupService {
    /// Create a full backup of a database.
    ///
    /// The database stays available during the backup (hot snapshot via
    /// MVCC checkpoint). The engine manages filenames and the manifest.
    pub async fn backup_database(
        databases: &DatabaseManager,
        db_name: &str,
        backup_dir: &Path,
    ) -> Result<types::BackupEntry, ServiceError> {
        let entry = databases.get_available(db_name)?;
        let db_name_owned = db_name.to_owned();
        let dir = db_backup_dir(backup_dir, db_name)?;

        std::fs::create_dir_all(&dir).map_err(|e| {
            ServiceError::Internal(format!("failed to create backup directory: {e}"))
        })?;

        let db = entry.db();
        let is_persistent = db.path().is_some();

        let is_read_only = databases.is_read_only();
        if is_persistent && !cfg!(target_os = "windows") && !is_read_only {
            let segment = tokio::task::spawn_blocking(move || db.backup_full(&dir))
                .await
                .map_err(|e| ServiceError::Internal(e.to_string()))?
                .map_err(|e| ServiceError::Internal(format!("backup failed: {e}")))?;

            tracing::info!(
                database = %db_name_owned,
                filename = %segment.filename,
                size_bytes = segment.size_bytes,
                epoch = %segment.end_epoch,
                "Full backup created"
            );

            Ok(segment_to_entry(segment, db_name_owned))
        } else {
            // In-memory databases don't have a file manager, fall back to save()
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let filename = format!("{db_name_owned}_{timestamp}.grafeo");
            let path = dir.join(&filename);
            tokio::task::spawn_blocking(move || db.save(&path))
                .await
                .map_err(|e| ServiceError::Internal(e.to_string()))?
                .map_err(|e| ServiceError::Internal(format!("backup failed: {e}")))?;

            let size_bytes = std::fs::metadata(dir.join(&filename))
                .map(|m| m.len())
                .unwrap_or(0);

            tracing::info!(
                database = %db_name_owned,
                filename = %filename,
                size_bytes,
                "In-memory backup created via save()"
            );

            Ok(types::BackupEntry {
                filename,
                database: db_name_owned,
                kind: "full".to_owned(),
                size_bytes,
                created_at: millis_to_iso(timestamp as u64),
                start_epoch: 0,
                end_epoch: 0,
                checksum: 0,
            })
        }
    }

    /// Restore a database from a backup file.
    ///
    /// The entry stays in the DashMap the entire time:
    /// 1. Mark as `Restoring` (incoming requests get 503)
    /// 2. Safety backup via `backup_full` (DB still open)
    /// 3. Close the old handle
    /// 4. Replace data files on disk
    /// 5. Open new handle from restored data
    /// 6. Swap via ArcSwap
    /// 7. Mark `Available`
    pub async fn restore_database(
        databases: &DatabaseManager,
        db_name: &str,
        backup_path: &Path,
        backup_dir: &Path,
    ) -> Result<(), ServiceError> {
        ensure_migrated(backup_dir);

        if databases.is_read_only() {
            return Err(ServiceError::ReadOnly);
        }

        let data_dir = databases.data_dir().ok_or_else(|| {
            ServiceError::BadRequest("restore requires persistent storage (--data-dir)".to_string())
        })?;

        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        if entry.db().path().is_none() {
            return Err(ServiceError::BadRequest(
                "cannot restore an in-memory database".to_string(),
            ));
        }

        if !backup_path.exists() {
            return Err(ServiceError::NotFound(format!(
                "backup file not found: {}",
                backup_path.display()
            )));
        }

        // Resolve the per-db backup subdirectory before transitioning state,
        // so validation errors don't orphan the entry in Restoring.
        let db_dir = db_backup_dir(backup_dir, db_name)?;

        if !entry.set_restoring() {
            return Err(ServiceError::Conflict(
                "database is already being restored".to_string(),
            ));
        }

        let (result, has_valid_handle) =
            Self::do_restore(&entry, db_name, backup_path, &db_dir, data_dir).await;

        if has_valid_handle {
            entry.set_available();
        }
        result
    }

    async fn do_restore(
        entry: &Arc<DatabaseEntry>,
        db_name: &str,
        backup_path: &Path,
        backup_dir: &Path,
        data_dir: &Path,
    ) -> (Result<(), ServiceError>, bool) {
        // 1. Safety backup via backup_full
        tracing::info!(database = %db_name, "Creating safety backup before restore");

        if let Err(e) = std::fs::create_dir_all(backup_dir) {
            return (
                Err(ServiceError::Internal(format!(
                    "failed to create backup directory: {e}"
                ))),
                true,
            );
        }

        // Manager-wide read-only is rejected in restore_database() before we get here,
        // so the only fallback cases here are in-memory and Windows.
        let safety_dir = backup_dir.to_path_buf();
        let safety_db = entry.db();
        let use_chain_api = safety_db.path().is_some() && !cfg!(target_os = "windows");
        let (save_result, safety_file) = if use_chain_api {
            let dir = safety_dir.clone();
            let result = tokio::task::spawn_blocking(move || {
                safety_db.backup_full(&dir).map(|seg| dir.join(&seg.filename))
            })
            .await;
            match result {
                Ok(Ok(path)) => (Ok(Ok(())), Some(path)),
                Ok(Err(e)) => (Ok(Err(e)), None),
                Err(e) => (Err(e), None),
            }
        } else {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let safety_path = safety_dir.join(format!("safety_{timestamp}.grafeo"));
            let path_clone = safety_path.clone();
            let result =
                tokio::task::spawn_blocking(move || safety_db.save(&safety_path)).await;
            match result {
                Ok(Ok(())) => (Ok(Ok(())), Some(path_clone)),
                Ok(Err(e)) => (Ok(Err(e)), None),
                Err(e) => (Err(e), None),
            }
        };
        match save_result {
            Err(e) => return (Err(ServiceError::Internal(e.to_string())), true),
            Ok(Err(e)) => {
                return (
                    Err(ServiceError::Internal(format!("safety backup failed: {e}"))),
                    true,
                );
            }
            Ok(Ok(_)) => {}
        }

        // 2. Close the old handle
        let old_db = entry.db();
        if let Err(e) = old_db.close() {
            tracing::warn!(database = %db_name, error = %e, "Error closing database for restore");
        }
        drop(old_db);

        // 3. Replace data files on disk
        let db_dir = data_dir.join(db_name);
        let db_file = db_dir.join("data.grafeo");

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
        let wal_dir = db_dir.join("data.grafeo.wal");
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

        // 4. Open backup and save to the persistent path
        let backup_owned = backup_path.to_path_buf();
        let db_file_clone = db_file.clone();
        let open_result = tokio::task::spawn_blocking(move || -> Result<GrafeoDB, String> {
            let backup_db = GrafeoDB::open(&backup_owned)
                .map_err(|e| format!("failed to open backup: {e}"))?;
            backup_db
                .save(&db_file_clone)
                .map_err(|e| format!("failed to save restored data: {e}"))?;
            backup_db.close().ok();
            GrafeoDB::open(db_file_clone.to_str().unwrap())
                .map_err(|e| format!("failed to open restored database: {e}"))
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r);

        match open_result {
            Ok(new_db) => {
                entry.swap_db(Arc::new(new_db));
                tracing::info!(database = %db_name, "Database restored from backup");
                (Ok(()), true)
            }
            Err(e) => {
                tracing::error!(
                    database = %db_name,
                    error = %e,
                    "Failed to restore, recovering from safety backup"
                );
                let recovered =
                    Self::recover_from_safety(entry, &db_file, backup_dir, safety_file.as_deref()).await;
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

    /// Attempt recovery from the safety backup after a failed restore.
    async fn recover_from_safety(
        entry: &Arc<DatabaseEntry>,
        db_file: &Path,
        backup_dir: &Path,
        known_safety_file: Option<&Path>,
    ) -> bool {
        // Use the exact safety file if known, otherwise fall back to guessing
        let safety_path = known_safety_file.map(|p| p.to_path_buf()).or_else(|| {
            match GrafeoDB::read_backup_manifest(backup_dir) {
                Ok(Some(m)) => m.latest_full().map(|s| backup_dir.join(&s.filename)),
                _ => None,
            }
            .or_else(|| {
                std::fs::read_dir(backup_dir)
                    .ok()?
                    .flatten()
                    .filter(|e| {
                        e.path()
                            .extension()
                            .is_some_and(|ext| ext == "grafeo")
                    })
                    .max_by_key(|e| e.metadata().ok().and_then(|m| m.modified().ok()))
                    .map(|e| e.path())
            })
        });

        let Some(safety) = safety_path else {
            return false;
        };

        let db_file_owned = db_file.to_path_buf();
        let recovery_result = tokio::task::spawn_blocking(move || {
            if db_file_owned.exists() {
                if db_file_owned.is_dir() {
                    let _ = std::fs::remove_dir_all(&db_file_owned);
                } else {
                    let _ = std::fs::remove_file(&db_file_owned);
                }
            }
            if let Ok(safety_db) = GrafeoDB::open(&safety) {
                let _ = safety_db.save(&db_file_owned);
                safety_db.close().ok();
            }
            GrafeoDB::open(db_file_owned.to_str().unwrap())
        })
        .await;

        match recovery_result {
            Ok(Ok(db)) => {
                entry.swap_db(Arc::new(db));
                true
            }
            _ => false,
        }
    }

    /// List backup segments, optionally filtered by database name.
    ///
    /// On first call, migrates any legacy backup files (`{db}_{timestamp}.grafeo`)
    /// from the root backup directory into per-database subdirectories.
    pub fn list_backups(
        db_name: Option<&str>,
        backup_dir: &Path,
    ) -> Result<Vec<types::BackupEntry>, ServiceError> {
        if !backup_dir.exists() {
            return Ok(vec![]);
        }

        // Migrate legacy backups from root to per-db subdirectories
        migrate_legacy_backups(backup_dir);

        if let Some(name) = db_name {
            let dir = db_backup_dir(backup_dir, name)?;
            return Self::list_from_manifest(&dir, name);
        }

        let mut all = Vec::new();
        let entries = std::fs::read_dir(backup_dir)
            .map_err(|e| ServiceError::Internal(format!("failed to read backup directory: {e}")))?;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if let Ok(mut backups) = Self::list_from_manifest(&path, name) {
                        all.append(&mut backups);
                    }
                }
            }
        }

        all.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(all)
    }

    fn list_from_manifest(
        dir: &Path,
        db_name: &str,
    ) -> Result<Vec<types::BackupEntry>, ServiceError> {
        if !dir.exists() {
            return Ok(vec![]);
        }
        let manifest = GrafeoDB::read_backup_manifest(dir)
            .map_err(|e| ServiceError::Internal(format!("failed to read backup manifest: {e}")))?;
        let manifest_filenames: std::collections::HashSet<String>;
        let mut entries = match manifest {
            Some(m) => {
                manifest_filenames = m.segments.iter().map(|s| s.filename.clone()).collect();
                m.segments
                    .into_iter()
                    .filter(|seg| seg.kind == BackupKind::Full)
                    .filter(|seg| dir.join(&seg.filename).exists())
                    .map(|seg| segment_to_entry(seg, db_name.to_owned()))
                    .collect::<Vec<_>>()
            }
            None => {
                manifest_filenames = std::collections::HashSet::new();
                vec![]
            }
        };

        // Include .grafeo files not tracked by the manifest (legacy or in-memory backups)
        if let Ok(dir_entries) = std::fs::read_dir(dir) {
            for entry in dir_entries.flatten() {
                let path = entry.path();
                let Some(fname) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if !fname.ends_with(".grafeo") || manifest_filenames.contains(fname) {
                    continue;
                }
                if let Ok(meta) = std::fs::metadata(&path) {
                    let created_ms = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    entries.push(types::BackupEntry {
                        filename: fname.to_owned(),
                        database: db_name.to_owned(),
                        kind: "full".to_owned(),
                        size_bytes: meta.len(),
                        created_at: millis_to_iso(created_ms),
                        start_epoch: 0,
                        end_epoch: 0,
                        checksum: 0,
                    });
                }
            }
        }

        entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(entries)
    }


    /// Delete a backup file from a specific database's backup directory.
    pub fn delete_backup(
        db_name: &str,
        filename: &str,
        backup_dir: &Path,
    ) -> Result<(), ServiceError> {
        ensure_migrated(backup_dir);

        if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
            return Err(ServiceError::BadRequest(
                "invalid backup filename".to_string(),
            ));
        }

        let dir = db_backup_dir(backup_dir, db_name)?;
        let path = dir.join(filename);
        if !path.exists() {
            return Err(ServiceError::NotFound(format!(
                "backup '{filename}' not found for database '{db_name}'"
            )));
        }

        std::fs::remove_file(&path)
            .map_err(|e| ServiceError::Internal(format!("failed to delete backup: {e}")))?;

        // Don't remove from manifest — the engine derives filenames from
        // segment count. list_backups filters out missing files.

        tracing::info!(database = %db_name, filename = %filename, "Backup deleted");
        Ok(())
    }

    /// Enforce retention: keep the N most recent full backups per database,
    /// delete older ones. Removes files and updates the manifest without
    /// renumbering segments (the engine derives filenames from segment count).
    pub fn enforce_retention(
        db_name: &str,
        backup_dir: &Path,
        keep: usize,
    ) -> Result<Vec<String>, ServiceError> {
        ensure_migrated(backup_dir);

        // keep=0 would delete everything including the backup just created.
        // Treat as "keep at least 1" to avoid accidental data loss.
        let keep = keep.max(1);

        let dir = db_backup_dir(backup_dir, db_name)?;
        let manifest = GrafeoDB::read_backup_manifest(&dir).ok().flatten();

        if let Some(ref m) = manifest {
            // Manifest-based retention: only count full backups whose files exist
            let live_full_indices: Vec<usize> = m
                .segments
                .iter()
                .enumerate()
                .filter(|(_, s)| s.kind == BackupKind::Full && dir.join(&s.filename).exists())
                .map(|(i, _)| i)
                .collect();

            let mut deleted = Vec::new();

            if live_full_indices.len() > keep {
                let cutoff_idx = live_full_indices[live_full_indices.len() - keep];
                let filenames_to_delete: std::collections::HashSet<String> =
                    m.segments[..cutoff_idx]
                        .iter()
                        .filter(|s| dir.join(&s.filename).exists())
                        .map(|s| s.filename.clone())
                        .collect();

                for filename in &filenames_to_delete {
                    let path = dir.join(filename);
                    if std::fs::remove_file(&path).is_ok() {
                        tracing::info!(filename = %filename, "Removed old backup (retention policy)");
                        deleted.push(filename.clone());
                    }
                }
            }

            // Prune untracked .grafeo files not in the manifest
            let manifest_filenames: std::collections::HashSet<&str> =
                m.segments.iter().map(|s| s.filename.as_str()).collect();
            if let Ok(dir_entries) = std::fs::read_dir(&dir) {
                let mut untracked: Vec<(String, std::time::SystemTime)> = dir_entries
                    .flatten()
                    .filter_map(|e| {
                        let path = e.path();
                        let fname = path.file_name()?.to_str()?.to_string();
                        if !fname.ends_with(".grafeo") || manifest_filenames.contains(fname.as_str()) {
                            return None;
                        }
                        let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
                        Some((fname, modified))
                    })
                    .collect();
                // Keep the newest `keep` untracked files too
                if untracked.len() > keep {
                    untracked.sort_by(|a, b| b.1.cmp(&a.1));
                    for (fname, _) in untracked.into_iter().skip(keep) {
                        if std::fs::remove_file(dir.join(&fname)).is_ok() {
                            tracing::info!(filename = %fname, "Removed untracked backup (retention)");
                            deleted.push(fname);
                        }
                    }
                }
            }

            return Ok(deleted);
        }

        // No manifest — file-based retention (legacy and in-memory backups).
        // Sort by modified time, delete oldest.
        let mut files: Vec<(String, std::time::SystemTime)> = std::fs::read_dir(&dir)
            .map_err(|e| ServiceError::Internal(format!("failed to read backup dir: {e}")))?
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                let fname = path.file_name()?.to_str()?.to_string();
                if !fname.ends_with(".grafeo") {
                    return None;
                }
                let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
                Some((fname, modified))
            })
            .collect();

        if files.len() <= keep {
            return Ok(vec![]);
        }

        // Sort newest first
        files.sort_by(|a, b| b.1.cmp(&a.1));

        let mut deleted = Vec::new();
        for (filename, _) in files.into_iter().skip(keep) {
            let path = dir.join(&filename);
            if std::fs::remove_file(&path).is_ok() {
                tracing::info!(filename = %filename, "Removed old backup (retention policy)");
                deleted.push(filename);
            }
        }

        Ok(deleted)
    }

}

/// Migrate legacy backup files from the root backup directory into per-database
/// subdirectories. Old files were named `{db}_{timestamp}.grafeo`.
fn migrate_legacy_backups(backup_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(backup_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(fname) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !fname.ends_with(".grafeo") {
            continue;
        }
        let stem = fname.strip_suffix(".grafeo").unwrap();
        let parts: Vec<&str> = stem.split('_').collect();
        if parts.len() < 7 {
            continue;
        }
        let ts_len = if parts.len() >= 8
            && parts[parts.len() - 7..]
                .iter()
                .all(|p| p.chars().all(|c| c.is_ascii_digit()))
        {
            7
        } else if parts[parts.len() - 6..]
            .iter()
            .all(|p| p.chars().all(|c| c.is_ascii_digit()))
        {
            6
        } else {
            continue;
        };
        let db_name = parts[..parts.len() - ts_len].join("_");
        if db_name.is_empty() {
            continue;
        }
        let target_dir = backup_dir.join(&db_name);
        if std::fs::create_dir_all(&target_dir).is_ok() {
            let target = target_dir.join(fname);
            if !target.exists() {
                if std::fs::rename(&path, &target).is_ok() {
                    tracing::info!(filename = %fname, database = %db_name, "Migrated legacy backup");
                }
            }
        }
    }
}

fn segment_to_entry(seg: BackupSegment, database: String) -> types::BackupEntry {
    let kind = match seg.kind {
        BackupKind::Full => "full",
        BackupKind::Incremental => "incremental",
        _ => "unknown",
    };
    types::BackupEntry {
        filename: seg.filename,
        database,
        kind: kind.to_owned(),
        size_bytes: seg.size_bytes,
        created_at: millis_to_iso(seg.created_at_ms),
        start_epoch: seg.start_epoch.0,
        end_epoch: seg.end_epoch.0,
        checksum: seg.checksum,
    }
}

fn millis_to_iso(ms: u64) -> String {
    let secs = ms / 1000;
    let millis = ms % 1000;
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}.{millis:03}Z")
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millis_to_iso_formats_correctly() {
        assert_eq!(
            millis_to_iso(1_705_282_245_123),
            "2024-01-15T01:30:45.123Z"
        );
    }

    #[test]
    fn days_to_ymd_epoch() {
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
    }

    #[test]
    fn days_to_ymd_known_date() {
        assert_eq!(days_to_ymd(19737), (2024, 1, 15));
    }

    #[tokio::test]
    async fn backup_and_list() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        let backup = BackupService::backup_database(&mgr, "default", backup_dir.path())
            .await
            .unwrap();
        assert_eq!(backup.database, "default");
        assert_eq!(backup.kind, "full");
        assert!(backup.size_bytes > 0);

        let list = BackupService::list_backups(Some("default"), backup_dir.path()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].kind, "full");
    }

    #[tokio::test]
    async fn backup_not_found() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        let result = BackupService::backup_database(&mgr, "nonexistent", backup_dir.path()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn list_backups_empty() {
        let backup_dir = tempfile::tempdir().unwrap();
        assert!(BackupService::list_backups(None, backup_dir.path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_backups_nonexistent_dir() {
        assert!(BackupService::list_backups(None, Path::new("/nonexistent")).unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_backup() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        let backup = BackupService::backup_database(&mgr, "default", backup_dir.path())
            .await
            .unwrap();
        BackupService::delete_backup("default", &backup.filename, backup_dir.path()).unwrap();
        assert!(BackupService::list_backups(Some("default"), backup_dir.path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_backup_path_traversal() {
        let backup_dir = tempfile::tempdir().unwrap();
        assert!(BackupService::delete_backup("default", "../etc/passwd", backup_dir.path()).is_err());
    }

    #[tokio::test]
    async fn delete_nonexistent_backup() {
        let backup_dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            BackupService::delete_backup("default", "nope.grafeo", backup_dir.path()),
            Err(ServiceError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn backup_creates_directory() {
        let data_dir = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("deeply").join("nested");
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        assert!(BackupService::backup_database(&mgr, "default", &nested).await.is_ok());
        assert!(nested.join("default").exists());
    }

    #[tokio::test]
    async fn restore_requires_persistent_storage() {
        let state = crate::ServiceState::new_in_memory(300);
        let backup_dir = tempfile::tempdir().unwrap();
        let dummy = backup_dir.path().join("dummy.grafeo");
        std::fs::write(&dummy, b"fake").unwrap();

        let result = BackupService::restore_database(
            state.databases(), "default", &dummy, backup_dir.path(),
        ).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn restore_with_persistent_storage() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        {
            let entry = mgr.get("default").unwrap();
            entry.db().session().execute("INSERT (:Person {name: 'Alice'})").unwrap();
            assert_eq!(entry.db().node_count(), 1);
        }

        let backup = BackupService::backup_database(&mgr, "default", backup_dir.path())
            .await
            .unwrap();

        {
            let entry = mgr.get("default").unwrap();
            entry.db().session().execute("INSERT (:Person {name: 'Bob'})").unwrap();
            assert_eq!(entry.db().node_count(), 2);
        }

        let file_path = db_backup_dir(backup_dir.path(), "default").unwrap().join(&backup.filename);
        BackupService::restore_database(
            &mgr, "default", &file_path, backup_dir.path(),
        ).await.unwrap();

        assert_eq!(mgr.get("default").unwrap().db().node_count(), 1);
    }

    #[tokio::test]
    async fn restore_read_only_rejected() {
        let mgr = crate::database::DatabaseManager::new(None, true);
        let backup_dir = tempfile::tempdir().unwrap();
        let dummy = backup_dir.path().join("dummy.grafeo");
        std::fs::write(&dummy, b"fake").unwrap();

        assert!(matches!(
            BackupService::restore_database(&mgr, "default", &dummy, backup_dir.path()).await,
            Err(ServiceError::ReadOnly)
        ));
    }

    #[tokio::test]
    async fn multi_database_isolation() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        let req = crate::types::CreateDatabaseRequest {
            name: "other".to_string(),
            database_type: crate::types::DatabaseType::Lpg,
            storage_mode: crate::types::StorageMode::Persistent,
            options: crate::types::DatabaseOptions::default(),
            schema_file: None,
            schema_filename: None,
        };
        mgr.create(&req).unwrap();

        BackupService::backup_database(&mgr, "default", backup_dir.path()).await.unwrap();
        BackupService::backup_database(&mgr, "other", backup_dir.path()).await.unwrap();

        let default_list = BackupService::list_backups(Some("default"), backup_dir.path()).unwrap();
        assert_eq!(default_list.len(), 1);
        assert_eq!(default_list[0].database, "default");

        let other_list = BackupService::list_backups(Some("other"), backup_dir.path()).unwrap();
        assert_eq!(other_list.len(), 1);
        assert_eq!(other_list[0].database, "other");

        assert_eq!(BackupService::list_backups(None, backup_dir.path()).unwrap().len(), 2);
    }
}
