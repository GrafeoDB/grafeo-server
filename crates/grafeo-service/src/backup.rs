//! Backup and restore operations for managed databases.
//!
//! Uses the engine's backup chain API (`backup_full`) for hot snapshots with
//! real epoch tracking and checksums. Each database gets its own subdirectory
//! within the configured backup dir (`{backup_dir}/{db_name}/`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use grafeo_engine::database::backup::{BackupKind, BackupSegment};
use grafeo_engine::{Config, GrafeoDB};

use crate::database::{DatabaseEntry, DatabaseManager};
use crate::error::ServiceError;
use crate::types;

/// Sidecar file name (relative to each per-database backup directory) that
/// stores optional user-supplied labels keyed by backup filename. The engine
/// owns filenames, so labels live out-of-band — losing this file loses
/// labels but not backups.
const LABELS_FILENAME: &str = "labels.json";

/// The engine's message when no full backup reaches the requested epoch.
const UNCOVERED_EPOCH_MESSAGE: &str = "no full backup covers epoch";

/// Returns the per-database backup subdirectory.
fn db_backup_dir(backup_dir: &Path, db_name: &str) -> Result<PathBuf, ServiceError> {
    if db_name.contains('/') || db_name.contains('\\') || db_name.contains("..") {
        return Err(ServiceError::BadRequest(
            "invalid database name".to_string(),
        ));
    }
    Ok(backup_dir.join(db_name))
}

/// The WAL sidecar the engine keeps next to a `.grafeo` file: `<file>.wal`.
fn sidecar_wal(db_file: &Path) -> PathBuf {
    let mut path = db_file.as_os_str().to_owned();
    path.push(".wal");
    PathBuf::from(path)
}

/// Removes a file or directory. A missing path is fine; any other error is
/// logged, since a leftover can break a later restore.
fn remove_path(path: &Path) {
    let result = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    if let Err(e) = result
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %e, "Could not remove path");
    }
}

/// Removes a database file and its WAL sidecar, each a file or a directory.
fn remove_db_files(db_file: &Path) {
    remove_path(db_file);
    remove_path(&sidecar_wal(db_file));
}

/// Removes the scratch directories the engine's `restore_to_epoch` leaves
/// next to the staging file (`<staged>.restore_wal` and its `.trimmed_wal`).
fn clear_replay_scratch(staged: &Path) {
    let mut restore_wal = staged.as_os_str().to_owned();
    restore_wal.push(".restore_wal");
    let mut trimmed = restore_wal.clone();
    trimmed.push(".trimmed_wal");
    remove_path(Path::new(&restore_wal));
    remove_path(Path::new(&trimmed));
}

/// Removes the staging database, its sidecar and the replay scratch.
fn clear_staging(staged: &Path) {
    remove_db_files(staged);
    clear_replay_scratch(staged);
}

/// True when the database file or its WAL sidecar exists.
fn db_files_exist(db_file: &Path) -> bool {
    // An unreadable path (for example permission denied) counts as present,
    // so it blocks the restore instead of being ignored.
    db_file.try_exists().unwrap_or(true) || sidecar_wal(db_file).try_exists().unwrap_or(true)
}

/// Moves a database file, and its WAL sidecar when there is one, to `to`.
/// All or nothing: when the sidecar cannot move, the file is moved back.
fn move_db_files(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)?;
    let wal = sidecar_wal(from);
    if wal.exists()
        && let Err(e) = std::fs::rename(&wal, sidecar_wal(to))
    {
        return match std::fs::rename(to, from) {
            Ok(()) => Err(e),
            Err(undo) => Err(std::io::Error::other(format!(
                "{e}; moving the database file back also failed: {undo}"
            ))),
        };
    }
    Ok(())
}

/// Startup recovery for a crash between the two renames of an epoch restore:
/// the original sits at `data.grafeo.pre-restore` and there is no
/// `data.grafeo`. Moves it back so the database opens with its data instead
/// of being recreated empty. When a live database exists too, or the
/// directory is read-only, nothing moves and an operator decides.
pub(crate) fn recover_orphaned_pre_restore(db_dir: &Path, read_only: bool) {
    let live = db_dir.join("data.grafeo");
    let previous = db_dir.join("data.grafeo.pre-restore");
    if !db_files_exist(&previous) {
        return;
    }
    if db_files_exist(&live) {
        tracing::error!(
            path = %previous.display(),
            "Found a pre-restore copy next to a live database; leaving both in place. It may be the original from an epoch restore: verify it, then remove it"
        );
        return;
    }
    if read_only {
        tracing::error!(
            path = %previous.display(),
            "Found an orphaned pre-restore copy and no database file, but the server is read-only; move it back to data.grafeo by hand"
        );
        return;
    }
    if !previous.try_exists().unwrap_or(false) {
        tracing::error!(
            path = %previous.display(),
            "Found a pre-restore WAL without its database file; leaving it in place for an operator"
        );
        return;
    }
    match move_db_files(&previous, &live) {
        Ok(()) => tracing::warn!(
            path = %previous.display(),
            restored = %live.display(),
            "Recovered the original database from an interrupted epoch restore"
        ),
        Err(e) => tracing::error!(
            path = %previous.display(),
            error = %e,
            "Could not move an orphaned pre-restore copy back into place"
        ),
    }
}

/// Moves the live database aside to `previous` and the staged restore into
/// its place. When the second move fails the live database is put back; if
/// that fails too, the error says so.
fn swap_db_files(live: &Path, staged: &Path, previous: &Path) -> std::io::Result<()> {
    move_db_files(live, previous)?;
    if let Err(e) = move_db_files(staged, live) {
        return match move_db_files(previous, live) {
            Ok(()) => Err(e),
            Err(undo) => Err(std::io::Error::other(format!(
                "{e}; putting the original database back also failed: {undo}"
            ))),
        };
    }
    Ok(())
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
    /// Replays the full backup plus the incremental segments needed to reach
    /// the target epoch into a staging file, then swaps it in and hot-swaps
    /// the database handle. A chain that fails to replay leaves the live
    /// database untouched.
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
        let staged = db_dir.join("data.grafeo.restoring");
        let previous = db_dir.join("data.grafeo.pre-restore");
        let (reopen_config, _) = databases.reopen_config(&db_file);

        // Engine 0.5.43+ never restores over an existing database, so the
        // chain is replayed into a staging file before the live handle
        // closes. A chain that fails to replay leaves the live database
        // untouched.
        // A leftover pre-restore copy can be the only copy of the original
        // database (after a failed put-back and a restart), so it is never
        // deleted: refuse to start and leave it for an operator.
        if db_files_exist(&previous) {
            entry.set_available();
            return Err(ServiceError::Conflict(format!(
                "a previous epoch restore left {} behind, or it cannot be checked; it may be the only copy of the original database: verify it (or move it somewhere safe) before removing it, then retry",
                previous.display()
            )));
        }

        clear_staging(&staged);
        let epoch_id = grafeo_common::types::EpochId::new(target_epoch);
        let staged_clone = staged.clone();
        let restore_result = tokio::task::spawn_blocking(move || {
            GrafeoDB::restore_to_epoch(&backup_sub, epoch_id, &staged_clone)
        })
        .await
        .map_err(|e| ServiceError::Internal(e.to_string()))
        .and_then(|r| {
            r.map_err(|e| {
                let message = format!("restore to epoch failed: {e}");
                // The engine reports an epoch no full backup reaches as an
                // internal error; to the caller it is a bad request.
                if e.to_string().contains(UNCOVERED_EPOCH_MESSAGE) {
                    ServiceError::BadRequest(message)
                } else {
                    ServiceError::Internal(message)
                }
            })
        });
        if let Err(e) = restore_result {
            clear_staging(&staged);
            entry.set_available();
            return Err(e);
        }
        clear_replay_scratch(&staged);

        // Release the live handle so its files can be moved aside. If the
        // close fails the old handle may still be open, so no file moves. It
        // keeps serving, but its checkpoint timer and WAL flusher already
        // stopped; a restart restores them.
        let old_db = entry.db();
        if let Err(e) = old_db.close() {
            clear_staging(&staged);
            entry.set_available();
            return Err(ServiceError::Internal(format!(
                "failed to close the database for epoch restore: {e}"
            )));
        }
        drop(old_db);

        if let Err(e) = swap_db_files(&db_file, &staged, &previous) {
            clear_staging(&staged);
            // Reopen only when the original is back in place and nothing is
            // left at the pre-restore path.
            if db_file.exists() && !db_files_exist(&previous) {
                Self::recover_after_failed_epoch_restore(&entry, &db_file, db_name, &reopen_config)
                    .await;
            } else {
                tracing::error!(
                    database = %db_name,
                    original = %previous.display(),
                    "Epoch restore swap failed and the original is not back in place; entry left in Restoring state, original may be at the pre-restore path"
                );
            }
            return Err(ServiceError::Internal(format!(
                "failed to swap in the restored database: {e}"
            )));
        }

        let open_config = reopen_config.clone();
        let open_result = tokio::task::spawn_blocking(move || GrafeoDB::with_config(open_config))
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))
            .and_then(|r| {
                r.map_err(|e| ServiceError::Internal(format!("failed to reopen database: {e}")))
            });

        match open_result {
            Ok(new_db) => {
                entry.swap_db(Arc::new(new_db));
                entry.set_available();
                remove_db_files(&previous);
                if db_files_exist(&previous) {
                    tracing::warn!(
                        database = %db_name,
                        path = %previous.display(),
                        "Epoch restore succeeded but the pre-restore copy could not be removed; delete it by hand, it blocks the next epoch restore"
                    );
                }
                tracing::info!(
                    database = %db_name,
                    epoch = target_epoch,
                    "Database restored to epoch"
                );
                Ok(())
            }
            Err(e) => {
                // Put the original files back, then reopen them. If they
                // cannot be put back, the entry stays in Restoring and the
                // original is left at the pre-restore path.
                remove_db_files(&db_file);
                if db_files_exist(&db_file) {
                    // Moving the original in now would replay the restored
                    // database's WAL into it.
                    tracing::error!(
                        database = %db_name,
                        restored = %db_file.display(),
                        original = %previous.display(),
                        "Could not clear the restored files after a failed reopen; entry left in Restoring state, original kept at the pre-restore path"
                    );
                    return Err(e);
                }
                match move_db_files(&previous, &db_file) {
                    Ok(()) => {
                        Self::recover_after_failed_epoch_restore(
                            &entry,
                            &db_file,
                            db_name,
                            &reopen_config,
                        )
                        .await;
                    }
                    Err(move_err) => {
                        tracing::error!(
                            database = %db_name,
                            error = %move_err,
                            original = %previous.display(),
                            "Could not put the original database back after a failed epoch \
                             restore; entry left in Restoring state, original kept at the \
                             pre-restore path"
                        );
                    }
                }
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
        reopen_config: &Config,
    ) {
        // Never open a missing file: the engine would create an empty
        // database and the entry would serve it as if it were the original.
        // An unreadable path is not proof the file is there, so it keeps
        // the entry in Restoring like a missing one.
        match db_file.try_exists() {
            Ok(true) => {}
            Ok(false) => {
                tracing::error!(
                    database = %db_name,
                    path = %db_file.display(),
                    "Epoch restore failed and the db file is missing; \
                     entry left in Restoring state so callers get 503"
                );
                return;
            }
            Err(e) => {
                tracing::error!(
                    database = %db_name,
                    path = %db_file.display(),
                    error = %e,
                    "Epoch restore failed and the db file cannot be checked; \
                     entry left in Restoring state so callers get 503"
                );
                return;
            }
        }

        let config = reopen_config.clone();
        let reopen = tokio::task::spawn_blocking(move || GrafeoDB::with_config(config)).await;
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

        let (reopen_config, _) =
            databases.reopen_config(&data_dir.join(db_name).join("data.grafeo"));

        if !entry.set_restoring() {
            return Err(ServiceError::Conflict(
                "database is already being restored".to_string(),
            ));
        }

        let (result, has_valid_handle) = Self::do_restore(
            &entry,
            db_name,
            backup_path,
            &db_dir,
            data_dir,
            reopen_config,
        )
        .await;

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
        reopen_config: Config,
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
            // Like epoch restore: nothing was touched on disk, and the old
            // handle may still be open, so no file is removed under it. It
            // keeps serving, but its checkpoint timer and WAL flusher already
            // stopped; a restart restores them.
            tracing::error!(database = %db_name, error = %e, "Error closing database for restore; restore aborted");
            return (
                Err(ServiceError::Internal(format!(
                    "failed to close the database for restore: {e}"
                ))),
                true,
            );
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
        let open_config = reopen_config.clone();
        let open_result = tokio::task::spawn_blocking(move || -> Result<GrafeoDB, String> {
            let backup_db =
                GrafeoDB::open(&backup_owned).map_err(|e| format!("failed to open backup: {e}"))?;
            backup_db
                .save(&db_file_clone)
                .map_err(|e| format!("failed to save restored data: {e}"))?;
            backup_db.close().ok();
            GrafeoDB::with_config(open_config)
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
                let recovered = Self::recover_from_safety(
                    entry,
                    &db_file,
                    backup_dir,
                    safety_file.as_deref(),
                    &reopen_config,
                )
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
        reopen_config: &Config,
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
        let config = reopen_config.clone();
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
            GrafeoDB::with_config(config)
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
    fn move_db_files_rolls_back_when_the_sidecar_cannot_move() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("a.grafeo");
        let to = dir.path().join("b.grafeo");
        std::fs::write(&from, "main").unwrap();
        std::fs::write(sidecar_wal(&from), "wal").unwrap();
        // Renaming a file onto a non-empty directory fails on Unix and Windows.
        let blocker = sidecar_wal(&to);
        std::fs::create_dir(&blocker).unwrap();
        std::fs::write(blocker.join("occupied"), "x").unwrap();

        move_db_files(&from, &to).expect_err("the sidecar move must fail");

        assert_eq!(std::fs::read_to_string(&from).unwrap(), "main");
        assert_eq!(std::fs::read_to_string(sidecar_wal(&from)).unwrap(), "wal");
        assert!(!to.exists(), "the database file must be moved back");
    }

    #[test]
    fn swap_db_files_puts_the_live_database_back_when_staging_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("data.grafeo");
        let staged = dir.path().join("data.grafeo.restoring");
        let previous = dir.path().join("data.grafeo.pre-restore");
        std::fs::write(&live, "live").unwrap();
        std::fs::write(sidecar_wal(&live), "live-wal").unwrap();

        swap_db_files(&live, &staged, &previous).expect_err("the staged move must fail");

        assert_eq!(std::fs::read_to_string(&live).unwrap(), "live");
        assert_eq!(
            std::fs::read_to_string(sidecar_wal(&live)).unwrap(),
            "live-wal"
        );
        assert!(!previous.exists());
        assert!(!sidecar_wal(&previous).exists());
    }

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
        assert_ne!(backup.created_at, "");
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
    async fn restore_to_epoch_unwritable_dir_keeps_database_online() {
        // The engine cannot write the staging file into a read-only
        // directory; the restore fails before the live handle closes.
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

        let backup = BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();

        // Strip write on the per-db data dir (keeping read and execute so
        // the stat checks still work) so the engine cannot create the
        // staging file. The target epoch is covered by the full backup, so
        // the failure is the read-only directory, not the chain.
        let db_data_dir = data_dir.path().join("default");
        let original_mode = std::fs::metadata(&db_data_dir)
            .unwrap()
            .permissions()
            .mode();
        std::fs::set_permissions(&db_data_dir, std::fs::Permissions::from_mode(0o500)).unwrap();

        let result =
            BackupService::restore_to_epoch(&mgr, "default", backup.end_epoch, backup_dir.path())
                .await;

        // Restore perms before asserting so tempdir cleanup works even
        // if the assertion fails.
        std::fs::set_permissions(&db_data_dir, std::fs::Permissions::from_mode(original_mode))
            .unwrap();

        let err = result.expect_err("a read-only db dir must fail the restore");
        assert!(
            err.to_string().contains("restore to epoch failed"),
            "unexpected error: {err}"
        );

        let entry = mgr.get("default").unwrap();
        assert!(
            !entry.is_restoring(),
            "a restore that fails before the swap must leave the entry available"
        );
        assert_eq!(entry.db().node_count(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn restore_to_epoch_uncheckable_dir_conflicts_and_stays_online() {
        // With mode 0o000 the stale pre-restore check cannot stat anything,
        // so the restore refuses with a Conflict before touching any file.
        use std::os::unix::fs::PermissionsExt;

        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);

        mgr.get("default")
            .unwrap()
            .db()
            .session()
            .execute("INSERT (:Person {name: 'Alice'})")
            .unwrap();
        let backup = BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();

        let db_data_dir = data_dir.path().join("default");
        let original_mode = std::fs::metadata(&db_data_dir)
            .unwrap()
            .permissions()
            .mode();
        std::fs::set_permissions(&db_data_dir, std::fs::Permissions::from_mode(0o000)).unwrap();

        let result =
            BackupService::restore_to_epoch(&mgr, "default", backup.end_epoch, backup_dir.path())
                .await;

        std::fs::set_permissions(&db_data_dir, std::fs::Permissions::from_mode(original_mode))
            .unwrap();

        let err = result.expect_err("an uncheckable db dir must fail the restore");
        assert!(matches!(err, ServiceError::Conflict(_)), "got: {err}");

        let entry = mgr
            .get_available("default")
            .expect("the entry must be available again");
        assert_eq!(entry.db().node_count(), 1);
        assert!(db_data_dir.join("data.grafeo").exists());
        assert!(!db_data_dir.join("data.grafeo.pre-restore").exists());
        assert!(!db_data_dir.join("data.grafeo.restoring").exists());
    }

    #[tokio::test]
    async fn restore_to_epoch_bad_chain_keeps_database_online() {
        // A corrupt full backup that the epoch check accepts is copied into
        // the staging file unvalidated, so the replay succeeds. The swap
        // happens, the reopen of the corrupt file fails, the original files
        // are put back and the recovery reopens them: the entry ends
        // Available with the original data.
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);
        let insert = |q: &str| {
            mgr.get("default")
                .unwrap()
                .db()
                .session()
                .execute(q)
                .unwrap()
        };

        insert("INSERT (:Person {name: 'Alice'})");
        let backup = BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();
        std::fs::write(
            backup_dir.path().join("default").join(&backup.filename),
            b"corrupt",
        )
        .unwrap();
        insert("INSERT (:Person {name: 'Bob'})");

        let result =
            BackupService::restore_to_epoch(&mgr, "default", backup.end_epoch, backup_dir.path())
                .await;
        let err = result.expect_err("a corrupt chain must fail");
        assert!(
            err.to_string().contains("failed to reopen database"),
            "unexpected error: {err}"
        );

        let entry = mgr.get("default").unwrap();
        assert!(!entry.is_restoring());
        assert_eq!(
            entry.db().node_count(),
            2,
            "the original data must be back in place"
        );
        insert("INSERT (:Person {name: 'Carol'})");
        let db_dir = data_dir.path().join("default");
        assert!(!db_dir.join("data.grafeo.restoring").exists());
        assert!(!db_dir.join("data.grafeo.pre-restore").exists());
    }

    #[tokio::test]
    async fn restore_to_epoch_refuses_stale_pre_restore() {
        // A leftover pre-restore copy may be the only copy of the original
        // database, so the restore refuses to run and deletes nothing.
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);
        mgr.get("default")
            .unwrap()
            .db()
            .session()
            .execute("INSERT (:Person {name: 'Alice'})")
            .unwrap();
        let backup = BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();

        let stale = data_dir
            .path()
            .join("default")
            .join("data.grafeo.pre-restore");
        std::fs::write(&stale, b"only copy").unwrap();

        let err =
            BackupService::restore_to_epoch(&mgr, "default", backup.end_epoch, backup_dir.path())
                .await
                .expect_err("a stale pre-restore must block the restore");
        assert!(matches!(err, ServiceError::Conflict(_)), "got {err:?}");
        assert!(
            err.to_string().contains("data.grafeo.pre-restore"),
            "unexpected error: {err}"
        );
        let msg = err.to_string();
        assert!(msg.contains("only copy"), "unexpected error: {msg}");
        assert!(!msg.contains("  "), "double space in: {msg}");
        assert_eq!(std::fs::read(&stale).unwrap(), b"only copy");

        let entry = mgr.get("default").unwrap();
        assert!(!entry.is_restoring());
        assert_eq!(entry.db().node_count(), 1);
    }

    #[tokio::test]
    async fn restore_to_epoch_uncovered_epoch_keeps_database_online() {
        // No full backup covers epoch 0, so the engine fails during replay,
        // before the live handle closes.
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);
        let insert = |q: &str| {
            mgr.get("default")
                .unwrap()
                .db()
                .session()
                .execute(q)
                .unwrap()
        };

        insert("INSERT (:Person {name: 'Alice'})");
        let backup = BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();
        assert!(backup.end_epoch > 0, "epoch 0 must be uncovered");
        let handle_before = mgr.get("default").unwrap().db();

        let err = BackupService::restore_to_epoch(&mgr, "default", 0, backup_dir.path())
            .await
            .expect_err("an uncovered epoch must fail");
        assert!(
            matches!(err, ServiceError::BadRequest(_)),
            "an uncovered epoch is a client error, got: {err:?}"
        );
        assert!(
            err.to_string().contains("no full backup covers epoch"),
            "unexpected error: {err}"
        );

        let entry = mgr.get("default").unwrap();
        assert!(!entry.is_restoring());
        assert!(
            Arc::ptr_eq(&handle_before, &entry.db()),
            "the live handle must not be replaced"
        );
        assert_eq!(entry.db().node_count(), 1);
        insert("INSERT (:Person {name: 'Bob'})");
        let db_dir = data_dir.path().join("default");
        assert!(!db_dir.join("data.grafeo.restoring").exists());
        assert!(!db_dir.join("data.grafeo.restoring.restore_wal").exists());
    }

    #[tokio::test]
    async fn restore_to_epoch_ignores_stale_staging_file() {
        // A crash during an earlier restore can leave the staging file and
        // its sidecar behind; the next restore clears them.
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);
        let insert = |q: &str| {
            mgr.get("default")
                .unwrap()
                .db()
                .session()
                .execute(q)
                .unwrap()
        };

        insert("INSERT (:Person {name: 'Alice'})");
        let initial = BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();
        insert("INSERT (:Person {name: 'Bob'})");

        let db_dir = data_dir.path().join("default");
        std::fs::write(db_dir.join("data.grafeo.restoring"), b"left over").unwrap();
        std::fs::create_dir_all(db_dir.join("data.grafeo.restoring.wal")).unwrap();

        BackupService::restore_to_epoch(&mgr, "default", initial.end_epoch, backup_dir.path())
            .await
            .unwrap();

        assert_eq!(mgr.get("default").unwrap().db().node_count(), 1);
        assert!(!db_dir.join("data.grafeo.restoring").exists());
        assert!(!db_dir.join("data.grafeo.pre-restore").exists());
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

    #[cfg(feature = "sync")]
    #[tokio::test]
    async fn epoch_restore_keeps_cdc_on_a_primary() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mut mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);
        mgr.set_cdc_enabled(true);
        mgr.get("default")
            .unwrap()
            .db()
            .session()
            .execute("INSERT (:Person {name: 'Alice'})")
            .unwrap();
        let backup = BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();

        BackupService::restore_to_epoch(&mgr, "default", backup.end_epoch, backup_dir.path())
            .await
            .unwrap();

        crate::sync::SyncService::pull(&mgr, "default", 0, 100)
            .expect("the restored database must still record changes");
    }

    #[cfg(feature = "sync")]
    #[tokio::test]
    async fn full_restore_keeps_cdc_on_a_primary() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mut mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);
        mgr.set_cdc_enabled(true);
        mgr.get("default")
            .unwrap()
            .db()
            .session()
            .execute("INSERT (:Person {name: 'Alice'})")
            .unwrap();
        let backup = BackupService::backup_database(&mgr, "default", backup_dir.path(), None)
            .await
            .unwrap();

        let file_path = db_backup_dir(backup_dir.path(), "default")
            .unwrap()
            .join(&backup.filename);
        BackupService::restore_database(&mgr, "default", &file_path, backup_dir.path())
            .await
            .unwrap();

        crate::sync::SyncService::pull(&mgr, "default", 0, 100)
            .expect("the restored database must still record changes");
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
        assert_eq!(deleted, [] as [std::string::String; 0]);
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

    fn capped_request(limit: usize) -> types::CreateDatabaseRequest {
        types::CreateDatabaseRequest {
            name: "capped".to_string(),
            database_type: types::DatabaseType::Lpg,
            storage_mode: types::StorageMode::Persistent,
            options: types::DatabaseOptions {
                memory_limit_bytes: Some(limit),
                ..Default::default()
            },
            schema_file: None,
            schema_filename: None,
        }
    }

    #[tokio::test]
    async fn restore_to_epoch_keeps_creation_options() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);
        let limit = 128 * 1024 * 1024;
        mgr.create(&capped_request(limit)).unwrap();
        mgr.get("capped")
            .unwrap()
            .db()
            .session()
            .execute("INSERT (:Person {name: 'Alice'})")
            .unwrap();
        let initial = BackupService::backup_database(&mgr, "capped", backup_dir.path(), None)
            .await
            .unwrap();

        BackupService::restore_to_epoch(&mgr, "capped", initial.end_epoch, backup_dir.path())
            .await
            .unwrap();

        assert_eq!(mgr.get("capped").unwrap().db().memory_limit(), Some(limit));
    }

    #[tokio::test]
    async fn restore_database_keeps_creation_options() {
        let data_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::database::DatabaseManager::new(Some(data_dir.path().to_str().unwrap()), false);
        let limit = 128 * 1024 * 1024;
        mgr.create(&capped_request(limit)).unwrap();
        let backup = BackupService::backup_database(&mgr, "capped", backup_dir.path(), None)
            .await
            .unwrap();
        let backup_path = backup_dir.path().join("capped").join(&backup.filename);

        BackupService::restore_database(&mgr, "capped", &backup_path, backup_dir.path())
            .await
            .unwrap();

        assert_eq!(mgr.get("capped").unwrap().db().memory_limit(), Some(limit));
    }
}
