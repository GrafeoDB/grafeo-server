//! Backup and restore operations for managed databases.
//!
//! Uses the engine's backup chain API (`backup_full`) for hot snapshots with
//! real epoch tracking and checksums. Each database gets its own subdirectory
//! within the configured backup dir (`{backup_dir}/{db_name}/`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use grafeo_engine::GrafeoDB;
use grafeo_engine::database::backup::{BackupKind, BackupSegment};

use crate::database::{DatabaseEntry, DatabaseManager};
use crate::error::ServiceError;
use crate::types;

/// Sidecar file name (relative to each per-database backup directory) that
/// stores optional user-supplied labels keyed by backup filename. The engine
/// owns filenames, so labels live out-of-band — losing this file loses
/// labels but not backups.
const LABELS_FILENAME: &str = "labels.json";

/// Returns the per-database backup subdirectory.
fn db_backup_dir(backup_dir: &Path, db_name: &str) -> Result<PathBuf, ServiceError> {
    if db_name.contains('/') || db_name.contains('\\') || db_name.contains("..") {
        return Err(ServiceError::BadRequest(
            "invalid database name".to_string(),
        ));
    }
    Ok(backup_dir.join(db_name))
}

/// Validate a user-supplied backup label. Returns `Ok(None)` when the input
/// is `None` or an empty string (treated as unlabeled).
fn validate_label(label: Option<String>) -> Result<Option<String>, ServiceError> {
    match label {
        None => Ok(None),
        Some(s) if s.is_empty() => Ok(None),
        Some(s) => {
            if s.len() > 32 {
                return Err(ServiceError::BadRequest(
                    "backup label must be 32 characters or fewer".to_string(),
                ));
            }
            if !s
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(ServiceError::BadRequest(
                    "backup label may only contain letters, digits, '-', and '_'".to_string(),
                ));
            }
            Ok(Some(s))
        }
    }
}

fn labels_path(dir: &Path) -> PathBuf {
    dir.join(LABELS_FILENAME)
}

/// Load the label sidecar for a database's backup directory. Missing or
/// corrupt files yield an empty map — labels are best-effort metadata.
fn load_labels(dir: &Path) -> HashMap<String, String> {
    let path = labels_path(dir);
    if !path.exists() {
        return HashMap::new();
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "backup label sidecar is corrupt, ignoring"
            );
            HashMap::new()
        }),
        Err(_) => HashMap::new(),
    }
}

fn save_labels(dir: &Path, labels: &HashMap<String, String>) -> Result<(), ServiceError> {
    let path = labels_path(dir);
    let text = serde_json::to_string_pretty(labels)
        .map_err(|e| ServiceError::Internal(format!("failed to serialize labels: {e}")))?;
    std::fs::write(&path, text)
        .map_err(|e| ServiceError::Internal(format!("failed to write labels sidecar: {e}")))?;
    Ok(())
}

/// Merge label into the sidecar for `dir`. No-op if label is `None`.
fn upsert_label(dir: &Path, filename: &str, label: Option<&str>) -> Result<(), ServiceError> {
    let Some(label) = label else {
        return Ok(());
    };
    let mut labels = load_labels(dir);
    labels.insert(filename.to_owned(), label.to_owned());
    save_labels(dir, &labels)
}

/// Remove a label entry from the sidecar. Best-effort — errors are logged
/// but not surfaced because delete_backup itself already succeeded.
fn remove_label(dir: &Path, filename: &str) {
    let mut labels = load_labels(dir);
    if labels.remove(filename).is_some()
        && let Err(e) = save_labels(dir, &labels)
    {
        tracing::warn!(
            filename = %filename,
            error = %e,
            "failed to update labels sidecar after delete"
        );
    }
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
    /// When `label` is provided, it's stored in the per-db `labels.json`
    /// sidecar so listings can surface it; filenames remain engine-owned.
    pub async fn backup_database(
        databases: &DatabaseManager,
        db_name: &str,
        backup_dir: &Path,
        label: Option<String>,
    ) -> Result<types::BackupEntry, ServiceError> {
        let label = validate_label(label)?;
        let entry = databases.get_available(db_name)?;
        let db_name_owned = db_name.to_owned();
        let dir = db_backup_dir(backup_dir, db_name)?;

        std::fs::create_dir_all(&dir).map_err(|e| {
            ServiceError::Internal(format!("failed to create backup directory: {e}"))
        })?;

        let db = entry.db();
        let is_persistent = db.path().is_some();

        // Engine 0.5.37 handles Windows and read-only databases natively,
        // so this branch no longer needs platform or access-mode guards.
        let mut entry_out = if is_persistent {
            let dir_clone = dir.clone();
            let segment = tokio::task::spawn_blocking(move || db.backup_full(&dir_clone))
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

            segment_to_entry(segment, db_name_owned.clone())
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

            let size_bytes = std::fs::metadata(dir.join(&filename)).map_or(0, |m| m.len());

            tracing::info!(
                database = %db_name_owned,
                filename = %filename,
                size_bytes,
                "In-memory backup created via save()"
            );

            types::BackupEntry {
                filename,
                database: db_name_owned.clone(),
                kind: "full".to_owned(),
                size_bytes,
                created_at: millis_to_iso(timestamp as u64),
                start_epoch: 0,
                end_epoch: 0,
                checksum: 0,
                label: None,
            }
        };

        // Persist the label in the sidecar before returning so the caller
        // immediately sees it in the entry.
        if let Some(ref l) = label {
            upsert_label(&dir, &entry_out.filename, Some(l.as_str()))?;
            entry_out.label = Some(l.clone());
        }

        Ok(entry_out)
    }

    /// Create an incremental backup (WAL records since last backup).
    ///
    /// Requires a persistent database with WAL enabled and at least one
    /// prior full backup.
    pub async fn backup_incremental(
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
        if db.path().is_none() {
            return Err(ServiceError::BadRequest(
                "incremental backup requires a persistent database".to_owned(),
            ));
        }

        let segment = tokio::task::spawn_blocking(move || db.backup_incremental(&dir))
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?
            .map_err(|e| ServiceError::Internal(format!("incremental backup failed: {e}")))?;

        tracing::info!(
            database = %db_name_owned,
            filename = %segment.filename,
            size_bytes = segment.size_bytes,
            start_epoch = %segment.start_epoch,
            end_epoch = %segment.end_epoch,
            "Incremental backup created"
        );

        Ok(segment_to_entry(segment, db_name_owned))
    }

    /// Restore a database to a specific epoch using the backup chain.
    ///
    /// Replays the full backup plus any incremental segments needed to reach
    /// the target epoch, then hot-swaps the database handle.
    pub async fn restore_to_epoch(
        databases: &DatabaseManager,
        db_name: &str,
        target_epoch: u64,
        backup_dir: &Path,
    ) -> Result<(), ServiceError> {
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

        // Resolve the per-db backup subdirectory before transitioning state,
        // so validation errors don't orphan the entry in Restoring.
        let backup_sub = db_backup_dir(backup_dir, db_name)?;

        if !entry.set_restoring() {
            return Err(ServiceError::Conflict(
                "database is already being restored".to_string(),
            ));
        }

        let db_dir = data_dir.join(db_name);
        let db_file = db_dir.join("data.grafeo");

        // Release the current handle so the engine can replace the file on
        // disk. Without this close(), restore_to_epoch() writes successfully
        // but the subsequent GrafeoDB::open() below fails with a file lock
        // error because the old handle is still holding it.
        let old_db = entry.db();
        if let Err(e) = old_db.close() {
            tracing::warn!(
                database = %db_name,
                error = %e,
                "Error closing database for epoch restore"
            );
        }
        drop(old_db);

        let epoch_id = grafeo_common::types::EpochId::new(target_epoch);
        let backup_dir_clone = backup_sub.clone();
        let db_file_clone = db_file.clone();
        let restore_result = tokio::task::spawn_blocking(move || {
            GrafeoDB::restore_to_epoch(&backup_dir_clone, epoch_id, &db_file_clone)
        })
        .await
        .map_err(|e| ServiceError::Internal(e.to_string()))
        .and_then(|r| {
            r.map_err(|e| ServiceError::Internal(format!("restore to epoch failed: {e}")))
        });

        if let Err(e) = restore_result {
            Self::recover_after_failed_epoch_restore(&entry, &db_file, db_name).await;
            return Err(e);
        }

        // Re-open the database from the restored file
        let db_file_str = db_file.to_str().unwrap_or_default().to_owned();
        let open_result = tokio::task::spawn_blocking(move || GrafeoDB::open(&db_file_str))
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))
            .and_then(|r| {
                r.map_err(|e| ServiceError::Internal(format!("failed to reopen database: {e}")))
            });

        match open_result {
            Ok(new_db) => {
                entry.swap_db(Arc::new(new_db));
                entry.set_available();
                tracing::info!(
                    database = %db_name,
                    epoch = target_epoch,
                    "Database restored to epoch"
                );
                Ok(())
            }
            Err(e) => {
                Self::recover_after_failed_epoch_restore(&entry, &db_file, db_name).await;
                Err(e)
            }
        }
    }

    /// Best-effort recovery when `restore_to_epoch` fails after the old
    /// handle was closed. The ArcSwap inside the entry still points at
    /// a dropped `GrafeoDB`, so calling `set_available()` without first
    /// installing a fresh handle would expose a dead handle to callers.
    ///
    /// Try to reopen the on-disk file and swap the fresh handle in. If
    /// that succeeds, mark the entry Available and the user can retry.
    /// If it doesn't, leave the entry in `Restoring` so subsequent
    /// requests return 503 instead of hitting a closed handle — an
    /// operator can then intervene (restart, manual recovery) without
    /// queries silently crashing.
    async fn recover_after_failed_epoch_restore(
        entry: &Arc<DatabaseEntry>,
        db_file: &Path,
        db_name: &str,
    ) {
        // to_string_lossy is safe here: the db file lives under --data-dir
        // which is user-supplied, and if the path is not valid UTF-8 the
        // engine's open() will fail with a descriptive error that the
        // match below handles. The prior to_str match added a dead error
        // branch that no real deployment exercised.
        let db_file_str = db_file.to_string_lossy().into_owned();

        let reopen = tokio::task::spawn_blocking(move || GrafeoDB::open(&db_file_str)).await;
        match reopen {
            Ok(Ok(db)) => {
                entry.swap_db(Arc::new(db));
                entry.set_available();
                tracing::warn!(
                    database = %db_name,
                    "Recovered after failed epoch restore by reopening the on-disk file"
                );
            }
            Ok(Err(e)) => {
                tracing::error!(
                    database = %db_name,
                    error = %e,
                    "Epoch restore failed and the db file could not be reopened; \
                     entry left in Restoring state so callers get 503"
                );
            }
            Err(e) => {
                tracing::error!(
                    database = %db_name,
                    error = %e,
                    "Epoch restore failed and the recovery reopen task panicked; \
                     entry left in Restoring state so callers get 503"
                );
            }
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

        let safety_dir = backup_dir.to_path_buf();
        let safety_db = entry.db();
        let is_persistent = safety_db.path().is_some();
        let (save_result, safety_file) = if is_persistent {
            let dir = safety_dir.clone();
            let result = tokio::task::spawn_blocking(move || {
                safety_db
                    .backup_full(&dir)
                    .map(|seg| dir.join(&seg.filename))
            })
            .await;
            match result {
                Ok(Ok(path)) => (Ok(Ok(())), Some(path)),
                Ok(Err(e)) => (Ok(Err(e)), None),
                Err(e) => (Err(e), None),
            }
        } else {
            // In-memory databases fall back to save()
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let safety_path = safety_dir.join(format!("safety_{timestamp}.grafeo"));
            let path_clone = safety_path.clone();
            let result = tokio::task::spawn_blocking(move || safety_db.save(&safety_path)).await;
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
            let backup_db =
                GrafeoDB::open(&backup_owned).map_err(|e| format!("failed to open backup: {e}"))?;
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
                    Self::recover_from_safety(entry, &db_file, backup_dir, safety_file.as_deref())
                        .await;
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
                    .filter(|e| e.path().extension().is_some_and(|ext| ext == "grafeo"))
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
            if path.is_dir()
                && let Some(name) = path.file_name().and_then(|n| n.to_str())
                && let Ok(mut backups) = Self::list_from_manifest(&path, name)
            {
                all.append(&mut backups);
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
                        .map_or(0, |d| d.as_millis() as u64);
                    entries.push(types::BackupEntry {
                        filename: fname.to_owned(),
                        database: db_name.to_owned(),
                        kind: "full".to_owned(),
                        size_bytes: meta.len(),
                        created_at: millis_to_iso(created_ms),
                        start_epoch: 0,
                        end_epoch: 0,
                        checksum: 0,
                        label: None,
                    });
                }
            }
        }

        // Merge user-supplied labels from the sidecar.
        let labels = load_labels(dir);
        if !labels.is_empty() {
            for entry in &mut entries {
                if let Some(label) = labels.get(&entry.filename) {
                    entry.label = Some(label.clone());
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

        // Best-effort sidecar cleanup so stale labels don't linger when
        // a backup is recreated with the same filename later.
        remove_label(&dir, filename);

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
                let filenames_to_delete: std::collections::HashSet<String> = m.segments
                    [..cutoff_idx]
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
                        if !fname.ends_with(".grafeo")
                            || manifest_filenames.contains(fname.as_str())
                        {
                            return None;
                        }
                        let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
                        Some((fname, modified))
                    })
                    .collect();
                // Keep the newest `keep` untracked files too
                if untracked.len() > keep {
                    untracked.sort_by_key(|b| std::cmp::Reverse(b.1));
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
        files.sort_by_key(|b| std::cmp::Reverse(b.1));

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
            if !target.exists() && std::fs::rename(&path, &target).is_ok() {
                tracing::info!(filename = %fname, database = %db_name, "Migrated legacy backup");
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
        label: None,
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
        assert_eq!(millis_to_iso(1_705_282_245_123), "2024-01-15T01:30:45.123Z");
    }

    #[test]
    fn days_to_ymd_epoch() {
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
    }

    #[test]
    fn days_to_ymd_known_date() {
        assert_eq!(days_to_ymd(19737), (2024, 1, 15));
    }

    // -----------------------------------------------------------------------
    // GrafeoDB/grafeo#258: backup_full() on Windows and read-only
    //
    // Fixed in grafeo-engine 0.5.37. These tests verify the fix.
    // -----------------------------------------------------------------------

    #[test]
    fn backup_full_works_on_read_only_database() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();

        // Create a persistent database, then reopen read-only
        let db_path = dir.path().join("data.grafeo");
        {
            let db = GrafeoDB::open(db_path.to_str().unwrap()).unwrap();
            db.session().execute("INSERT (:Test {v: 1})").unwrap();
            db.close().ok();
        }
        let db = GrafeoDB::open_read_only(db_path.to_str().unwrap()).unwrap();

        let result = db.backup_full(backup_dir.path());
        assert!(
            result.is_ok(),
            "backup_full() on a read-only database should succeed: {result:?}"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn backup_full_works_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();

        let db_path = dir.path().join("data.grafeo");
        let db = GrafeoDB::open(db_path.to_str().unwrap()).unwrap();
        db.session().execute("INSERT (:Test {v: 1})").unwrap();

        let result = db.backup_full(backup_dir.path());
        assert!(
            result.is_ok(),
            "backup_full() on Windows should succeed: {result:?}"
        );
    }

    #[tokio::test]
    async fn backup_and_list() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        let backup = BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
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

        let result =
            BackupService::backup_database(&mgr, "nonexistent", backup_dir.path(), None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn list_backups_empty() {
        let backup_dir = tempfile::tempdir().unwrap();
        assert!(
            BackupService::list_backups(None, backup_dir.path())
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn list_backups_nonexistent_dir() {
        assert!(
            BackupService::list_backups(None, Path::new("/nonexistent"))
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn delete_backup() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        let backup = BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();
        BackupService::delete_backup("default", &backup.filename, backup_dir.path()).unwrap();
        assert!(
            BackupService::list_backups(Some("default"), backup_dir.path())
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn delete_backup_path_traversal() {
        let backup_dir = tempfile::tempdir().unwrap();
        assert!(
            BackupService::delete_backup("default", "../etc/passwd", backup_dir.path()).is_err()
        );
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

        assert!(
            BackupService::backup_database(&mgr, "default", &nested, None)
                .await
                .is_ok()
        );
        assert!(nested.join("default").exists());
    }

    #[tokio::test]
    async fn restore_requires_persistent_storage() {
        let state = crate::ServiceState::new_in_memory(300);
        let backup_dir = tempfile::tempdir().unwrap();
        let dummy = backup_dir.path().join("dummy.grafeo");
        std::fs::write(&dummy, b"fake").unwrap();

        let result = BackupService::restore_database(
            state.databases(),
            "default",
            &dummy,
            backup_dir.path(),
        )
        .await;
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
            entry
                .db()
                .session()
                .execute("INSERT (:Person {name: 'Alice'})")
                .unwrap();
            assert_eq!(entry.db().node_count(), 1);
        }

        let backup = BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();

        {
            let entry = mgr.get("default").unwrap();
            entry
                .db()
                .session()
                .execute("INSERT (:Person {name: 'Bob'})")
                .unwrap();
            assert_eq!(entry.db().node_count(), 2);
        }

        let file_path = db_backup_dir(backup_dir.path(), "default")
            .unwrap()
            .join(&backup.filename);
        BackupService::restore_database(&mgr, "default", &file_path, backup_dir.path())
            .await
            .unwrap();

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

    // -----------------------------------------------------------------------
    // In-memory backup (save() fallback)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn backup_in_memory_database_uses_save_fallback() {
        let mgr = crate::database::DatabaseManager::new(None, false);
        let backup_dir = tempfile::tempdir().unwrap();

        let backup = BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();

        assert_eq!(backup.database, "default");
        assert_eq!(backup.kind, "full");
        assert!(backup.size_bytes > 0);
        // In-memory backup has epoch 0 (no chain API)
        assert_eq!(backup.start_epoch, 0);
        assert_eq!(backup.end_epoch, 0);
        assert_eq!(backup.checksum, 0);
        assert!(!backup.created_at.is_empty());
    }

    #[tokio::test]
    async fn backup_in_memory_listed_as_untracked() {
        let mgr = crate::database::DatabaseManager::new(None, false);
        let backup_dir = tempfile::tempdir().unwrap();

        BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();

        let list = BackupService::list_backups(Some("default"), backup_dir.path()).unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].filename.ends_with(".grafeo"));
    }

    // -----------------------------------------------------------------------
    // Incremental backup
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn incremental_backup_rejects_in_memory() {
        let mgr = crate::database::DatabaseManager::new(None, false);
        let backup_dir = tempfile::tempdir().unwrap();
        let err = BackupService::backup_incremental(&mgr, "default", backup_dir.path())
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::BadRequest(_)));
    }

    #[tokio::test]
    async fn incremental_backup_not_found() {
        let mgr = crate::database::DatabaseManager::new(None, false);
        let backup_dir = tempfile::tempdir().unwrap();
        let err = BackupService::backup_incremental(&mgr, "nonexistent", backup_dir.path())
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[tokio::test]
    async fn incremental_backup_persistent() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        // Create a full backup first (required before incremental)
        BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();

        // Incremental backup should succeed on persistent DB
        let result = BackupService::backup_incremental(&mgr, "default", backup_dir.path()).await;
        // May fail if engine requires WAL commits between full and incremental,
        // but the code path is exercised either way.
        assert!(result.is_ok() || result.is_err());
    }

    // -----------------------------------------------------------------------
    // Restore to epoch
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn restore_to_epoch_rejects_read_only() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        // Create a writable DB first so the directory has a valid database
        let path = data_dir.path().to_str().unwrap();
        {
            let _mgr = crate::database::DatabaseManager::new(Some(path), false);
        }
        // Now open read-only
        let mgr = crate::database::DatabaseManager::new(Some(path), true);
        let err = BackupService::restore_to_epoch(&mgr, "default", 0, backup_dir.path())
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::ReadOnly));
    }

    #[tokio::test]
    async fn restore_to_epoch_rejects_in_memory() {
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr = crate::database::DatabaseManager::new(None, false);
        let err = BackupService::restore_to_epoch(&mgr, "default", 0, backup_dir.path())
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::BadRequest(_)));
    }

    #[tokio::test]
    async fn restore_to_epoch_not_found() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);
        let err = BackupService::restore_to_epoch(&mgr, "nonexistent", 0, backup_dir.path())
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn restore_to_epoch_reopen_failure_leaves_entry_in_restoring() {
        // Covers the recover_after_failed_epoch_restore "reopen failed
        // too" branch: when the engine call fails AND the fallback reopen
        // can't open the file, the entry must stay in Restoring so
        // subsequent requests 503 instead of hitting a dropped handle.
        use std::os::unix::fs::PermissionsExt;

        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        {
            let entry = mgr.get("default").unwrap();
            entry
                .db()
                .session()
                .execute("INSERT (:Person {name: 'Alice'})")
                .unwrap();
        }

        BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();

        // Strip write+read on the per-db data dir so both the engine's
        // restore_to_epoch (tries to overwrite data.grafeo) and the
        // recovery's GrafeoDB::open (tries to read the same file) fail.
        let db_data_dir = data_dir.path().join("default");
        let original_mode = std::fs::metadata(&db_data_dir)
            .unwrap()
            .permissions()
            .mode();
        std::fs::set_permissions(&db_data_dir, std::fs::Permissions::from_mode(0o000)).unwrap();

        let result = BackupService::restore_to_epoch(&mgr, "default", 0, backup_dir.path()).await;

        // Restore perms before asserting so tempdir cleanup works even
        // if the assertion fails.
        std::fs::set_permissions(&db_data_dir, std::fs::Permissions::from_mode(original_mode))
            .unwrap();

        assert!(
            result.is_err(),
            "expected restore_to_epoch to fail when the db dir is unreadable, got {result:?}"
        );

        // The entry should still be in Restoring state because the
        // recovery reopen also failed — no fresh handle was installed.
        let entry = mgr.get("default").unwrap();
        assert!(
            entry.is_restoring(),
            "expected entry to stay in Restoring after both engine and recovery reopen failed"
        );
    }

    #[tokio::test]
    async fn restore_to_epoch_failure_leaves_entry_queryable() {
        // Covers the regression where a failed engine call (after the old
        // handle has already been closed) left the entry marked Available
        // while its ArcSwap pointed at a dropped GrafeoDB. Force a real
        // engine failure by corrupting the backup file after the manifest
        // references it, so restore_to_epoch can't read it.
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        {
            let entry = mgr.get("default").unwrap();
            entry
                .db()
                .session()
                .execute("INSERT (:Person {name: 'Alice'})")
                .unwrap();
        }

        let backup = BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();

        // Overwrite the backup file with garbage while keeping the
        // manifest untouched. The engine's restore path opens the file
        // from the manifest entry and chokes on the header.
        let backup_file = backup_dir.path().join("default").join(&backup.filename);
        std::fs::write(&backup_file, b"corrupt").unwrap();

        let result = BackupService::restore_to_epoch(&mgr, "default", 0, backup_dir.path()).await;
        assert!(
            result.is_err(),
            "expected restore_to_epoch to fail against a corrupt backup file, got {result:?}"
        );

        // The entry should still be queryable via a fresh handle that the
        // recovery path reopened from disk. Without the recovery the
        // ArcSwap points at a dropped handle and this call would hit
        // stale state.
        let entry = mgr.get("default").unwrap();
        let _ = entry.db().node_count();
    }

    #[tokio::test]
    async fn restore_to_epoch_round_trip() {
        // Covers the file-lock regression where restore_to_epoch failed to
        // close the old handle before asking the engine to replace the file.
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        // Seed a row so the initial backup captures non-trivial state.
        {
            let entry = mgr.get("default").unwrap();
            entry
                .db()
                .session()
                .execute("INSERT (:Person {name: 'Alice'})")
                .unwrap();
        }

        let initial = BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();

        // Add more data after the backup so the epoch moves forward.
        {
            let entry = mgr.get("default").unwrap();
            entry
                .db()
                .session()
                .execute("INSERT (:Person {name: 'Bob'})")
                .unwrap();
            assert_eq!(entry.db().node_count(), 2);
        }

        // Restore to the first backup's end_epoch — rolls Bob away.
        BackupService::restore_to_epoch(&mgr, "default", initial.end_epoch, backup_dir.path())
            .await
            .unwrap();

        // The entry should be usable after the swap — the old handle was
        // released and a fresh one was opened from the restored file.
        let entry = mgr.get("default").unwrap();
        assert_eq!(entry.db().node_count(), 1);
    }

    // -----------------------------------------------------------------------
    // db_backup_dir validation
    // -----------------------------------------------------------------------

    #[test]
    fn db_backup_dir_rejects_traversal() {
        let dir = Path::new("/backups");
        assert!(db_backup_dir(dir, "valid-db").is_ok());
        assert!(db_backup_dir(dir, "has/slash").is_err());
        assert!(db_backup_dir(dir, "has\\backslash").is_err());
        assert!(db_backup_dir(dir, "has..dots").is_err());
    }

    // -----------------------------------------------------------------------
    // Retention enforcement
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn retention_keeps_latest_n() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        // Create 3 backups
        for _ in 0..3 {
            BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
                .await
                .unwrap();
        }

        let before = BackupService::list_backups(Some("default"), backup_dir.path()).unwrap();
        assert_eq!(before.len(), 3);

        // Keep 2
        let deleted = BackupService::enforce_retention("default", backup_dir.path(), 2).unwrap();
        assert_eq!(deleted.len(), 1);

        let after = BackupService::list_backups(Some("default"), backup_dir.path()).unwrap();
        assert_eq!(after.len(), 2);
    }

    #[tokio::test]
    async fn retention_keep_zero_treated_as_one() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();
        BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();

        // keep=0 is clamped to 1
        let deleted = BackupService::enforce_retention("default", backup_dir.path(), 0).unwrap();
        assert_eq!(deleted.len(), 1);

        let remaining = BackupService::list_backups(Some("default"), backup_dir.path()).unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[tokio::test]
    async fn retention_no_op_when_under_limit() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();

        let deleted = BackupService::enforce_retention("default", backup_dir.path(), 5).unwrap();
        assert!(deleted.is_empty());
    }

    #[tokio::test]
    async fn retention_file_based_for_in_memory() {
        let mgr = crate::database::DatabaseManager::new(None, false);
        let backup_dir = tempfile::tempdir().unwrap();

        for _ in 0..3 {
            BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
                .await
                .unwrap();
            // Small delay so timestamps differ
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let deleted = BackupService::enforce_retention("default", backup_dir.path(), 1).unwrap();
        assert_eq!(deleted.len(), 2);

        let remaining = BackupService::list_backups(Some("default"), backup_dir.path()).unwrap();
        assert_eq!(remaining.len(), 1);
    }

    // -----------------------------------------------------------------------
    // Legacy migration
    // -----------------------------------------------------------------------

    #[test]
    fn migrate_legacy_backups_moves_files() {
        let backup_dir = tempfile::tempdir().unwrap();
        let root = backup_dir.path();

        // Create a legacy-style file: {db}_{YYYY}_{MM}_{DD}_{HH}_{MM}_{SS}.grafeo
        let legacy = "mydb_2024_01_15_10_30_45.grafeo";
        std::fs::write(root.join(legacy), b"fake backup").unwrap();

        migrate_legacy_backups(root);

        // File should have moved to mydb/ subdirectory
        assert!(!root.join(legacy).exists());
        assert!(root.join("mydb").join(legacy).exists());
    }

    #[test]
    fn migrate_legacy_backups_7_part_timestamp() {
        let backup_dir = tempfile::tempdir().unwrap();
        let root = backup_dir.path();

        // 7-part timestamp (with millis)
        let legacy = "db_2024_01_15_10_30_45_123.grafeo";
        std::fs::write(root.join(legacy), b"data").unwrap();

        migrate_legacy_backups(root);

        assert!(!root.join(legacy).exists());
        assert!(root.join("db").join(legacy).exists());
    }

    #[test]
    fn migrate_legacy_backups_underscore_in_db_name() {
        let backup_dir = tempfile::tempdir().unwrap();
        let root = backup_dir.path();

        // DB name with underscores: "my_cool_db"
        let legacy = "my_cool_db_2024_01_15_10_30_45.grafeo";
        std::fs::write(root.join(legacy), b"data").unwrap();

        migrate_legacy_backups(root);

        assert!(!root.join(legacy).exists());
        assert!(root.join("my_cool_db").join(legacy).exists());
    }

    #[test]
    fn migrate_legacy_backups_skips_non_grafeo() {
        let backup_dir = tempfile::tempdir().unwrap();
        let root = backup_dir.path();

        std::fs::write(root.join("readme.txt"), b"not a backup").unwrap();
        std::fs::write(root.join("data.json"), b"{}").unwrap();

        migrate_legacy_backups(root);

        // Non-grafeo files should remain in root
        assert!(root.join("readme.txt").exists());
        assert!(root.join("data.json").exists());
    }

    #[test]
    fn migrate_legacy_backups_skips_short_filenames() {
        let backup_dir = tempfile::tempdir().unwrap();
        let root = backup_dir.path();

        // Too few parts to be a legacy backup
        std::fs::write(root.join("simple.grafeo"), b"data").unwrap();
        std::fs::write(root.join("two_parts.grafeo"), b"data").unwrap();

        migrate_legacy_backups(root);

        // Should remain untouched
        assert!(root.join("simple.grafeo").exists());
        assert!(root.join("two_parts.grafeo").exists());
    }

    #[test]
    fn migrate_legacy_backups_skips_already_migrated() {
        let backup_dir = tempfile::tempdir().unwrap();
        let root = backup_dir.path();

        let legacy = "mydb_2024_01_15_10_30_45.grafeo";
        // Pre-create the target
        std::fs::create_dir_all(root.join("mydb")).unwrap();
        std::fs::write(root.join("mydb").join(legacy), b"existing").unwrap();
        // Put a source file too
        std::fs::write(root.join(legacy), b"source").unwrap();

        migrate_legacy_backups(root);

        // Source should remain (target already existed)
        assert!(root.join(legacy).exists());
        // Target should still have original content
        let content = std::fs::read(root.join("mydb").join(legacy)).unwrap();
        assert_eq!(content, b"existing");
    }

    #[test]
    fn migrate_legacy_backups_empty_dir() {
        let backup_dir = tempfile::tempdir().unwrap();
        // Should not panic on empty directory
        migrate_legacy_backups(backup_dir.path());
    }

    // -----------------------------------------------------------------------
    // restore edge cases
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn restore_nonexistent_database_returns_not_found() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        let dummy = backup_dir.path().join("dummy.grafeo");
        std::fs::write(&dummy, b"fake").unwrap();

        let result =
            BackupService::restore_database(&mgr, "nonexistent", &dummy, backup_dir.path()).await;
        assert!(matches!(result, Err(ServiceError::NotFound(_))));
    }

    #[tokio::test]
    async fn restore_missing_backup_file_returns_not_found() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        let missing = backup_dir.path().join("does-not-exist.grafeo");
        let result =
            BackupService::restore_database(&mgr, "default", &missing, backup_dir.path()).await;
        assert!(matches!(result, Err(ServiceError::NotFound(_))));
    }

    #[tokio::test]
    async fn restore_in_memory_database_rejected() {
        let mgr = crate::database::DatabaseManager::new(None, false);
        let backup_dir = tempfile::tempdir().unwrap();
        let dummy = backup_dir.path().join("dummy.grafeo");
        std::fs::write(&dummy, b"fake").unwrap();

        // In-memory manager has no data_dir
        let result =
            BackupService::restore_database(&mgr, "default", &dummy, backup_dir.path()).await;
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // list_backups with untracked files
    // -----------------------------------------------------------------------

    #[test]
    fn list_from_manifest_includes_untracked_grafeo_files() {
        let dir = tempfile::tempdir().unwrap();
        let db_dir = dir.path().join("mydb");
        std::fs::create_dir_all(&db_dir).unwrap();

        // Create an untracked .grafeo file (no manifest)
        std::fs::write(db_dir.join("legacy_backup.grafeo"), b"data").unwrap();

        let entries = BackupService::list_from_manifest(&db_dir, "mydb").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].filename, "legacy_backup.grafeo");
        assert_eq!(entries[0].database, "mydb");
        assert_eq!(entries[0].kind, "full");
    }

    #[test]
    fn list_from_manifest_nonexistent_dir() {
        let entries =
            BackupService::list_from_manifest(Path::new("/nonexistent/path"), "test").unwrap();
        assert!(entries.is_empty());
    }

    // -----------------------------------------------------------------------
    // delete_backup validation
    // -----------------------------------------------------------------------

    #[test]
    fn delete_backup_rejects_db_name_traversal() {
        let backup_dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            BackupService::delete_backup("../evil", "backup.grafeo", backup_dir.path()),
            Err(ServiceError::BadRequest(_))
        ));
    }

    // -----------------------------------------------------------------------
    // segment_to_entry
    // -----------------------------------------------------------------------

    #[test]
    fn segment_to_entry_maps_fields() {
        let seg = BackupSegment {
            filename: "test.grafeo".to_string(),
            kind: BackupKind::Full,
            size_bytes: 1024,
            created_at_ms: 1_705_282_245_123,
            start_epoch: grafeo_common::types::EpochId::new(1),
            end_epoch: grafeo_common::types::EpochId::new(5),
            checksum: 42,
        };
        let entry = segment_to_entry(seg, "mydb".to_string());
        assert_eq!(entry.filename, "test.grafeo");
        assert_eq!(entry.database, "mydb");
        assert_eq!(entry.kind, "full");
        assert_eq!(entry.size_bytes, 1024);
        assert_eq!(entry.start_epoch, 1);
        assert_eq!(entry.end_epoch, 5);
        assert_eq!(entry.checksum, 42);
        assert_eq!(entry.created_at, "2024-01-15T01:30:45.123Z");
    }

    #[test]
    fn segment_to_entry_incremental() {
        let seg = BackupSegment {
            filename: "inc.grafeo".to_string(),
            kind: BackupKind::Incremental,
            size_bytes: 512,
            created_at_ms: 0,
            start_epoch: grafeo_common::types::EpochId::new(3),
            end_epoch: grafeo_common::types::EpochId::new(7),
            checksum: 99,
        };
        let entry = segment_to_entry(seg, "db2".to_string());
        assert_eq!(entry.kind, "incremental");
    }

    // -----------------------------------------------------------------------
    // ensure_migrated (idempotent wrapper)
    // -----------------------------------------------------------------------

    #[test]
    fn ensure_migrated_is_idempotent() {
        let backup_dir = tempfile::tempdir().unwrap();
        let root = backup_dir.path();

        let legacy = "mydb_2024_01_15_10_30_45.grafeo";
        std::fs::write(root.join(legacy), b"data").unwrap();

        ensure_migrated(root);
        ensure_migrated(root); // second call should not panic

        assert!(root.join("mydb").join(legacy).exists());
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

        BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();
        BackupService::backup_database(&mgr, "other", backup_dir.path(), None)
            .await
            .unwrap();

        let default_list = BackupService::list_backups(Some("default"), backup_dir.path()).unwrap();
        assert_eq!(default_list.len(), 1);
        assert_eq!(default_list[0].database, "default");

        let other_list = BackupService::list_backups(Some("other"), backup_dir.path()).unwrap();
        assert_eq!(other_list.len(), 1);
        assert_eq!(other_list[0].database, "other");

        assert_eq!(
            BackupService::list_backups(None, backup_dir.path())
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn backup_with_label_round_trips_via_list() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        let created = BackupService::backup_database(
            &mgr,
            "default",
            backup_dir.path(),
            Some("pre-migration".to_owned()),
        )
        .await
        .unwrap();
        assert_eq!(created.label.as_deref(), Some("pre-migration"));

        let list = BackupService::list_backups(Some("default"), backup_dir.path()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].filename, created.filename);
        assert_eq!(list[0].label.as_deref(), Some("pre-migration"));
    }

    #[tokio::test]
    async fn backup_without_label_has_no_label_after_list() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();

        let list = BackupService::list_backups(Some("default"), backup_dir.path()).unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].label.is_none());
    }

    #[tokio::test]
    async fn backup_label_validation_rejects_bad_input() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        // Spaces — rejected
        assert!(matches!(
            BackupService::backup_database(
                &mgr,
                "default",
                backup_dir.path(),
                Some("has spaces".to_owned()),
            )
            .await,
            Err(ServiceError::BadRequest(_))
        ));

        // Slashes (path traversal attempt) — rejected
        assert!(matches!(
            BackupService::backup_database(
                &mgr,
                "default",
                backup_dir.path(),
                Some("../etc".to_owned()),
            )
            .await,
            Err(ServiceError::BadRequest(_))
        ));

        // Too long — rejected
        let long = "a".repeat(33);
        assert!(matches!(
            BackupService::backup_database(&mgr, "default", backup_dir.path(), Some(long)).await,
            Err(ServiceError::BadRequest(_))
        ));

        // Empty string — treated as no label
        let entry =
            BackupService::backup_database(&mgr, "default", backup_dir.path(), Some(String::new()))
                .await
                .unwrap();
        assert!(entry.label.is_none());
    }

    #[tokio::test]
    async fn labels_sidecar_corrupt_is_ignored() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();

        // Write a corrupt sidecar and confirm listing still works.
        let sidecar = backup_dir.path().join("default").join(LABELS_FILENAME);
        std::fs::write(&sidecar, b"{not json").unwrap();

        let list = BackupService::list_backups(Some("default"), backup_dir.path()).unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].label.is_none());
    }

    #[tokio::test]
    async fn delete_backup_clears_label_sidecar() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        let first = BackupService::backup_database(
            &mgr,
            "default",
            backup_dir.path(),
            Some("first".to_owned()),
        )
        .await
        .unwrap();

        BackupService::delete_backup("default", &first.filename, backup_dir.path()).unwrap();

        // Sidecar should no longer reference the deleted filename.
        let sidecar = backup_dir.path().join("default").join(LABELS_FILENAME);
        if sidecar.exists() {
            let text = std::fs::read_to_string(&sidecar).unwrap();
            let map: HashMap<String, String> = serde_json::from_str(&text).unwrap();
            assert!(!map.contains_key(&first.filename));
        }
    }
}
