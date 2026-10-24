//! Upgrading a grafeo-server 0.5.44 data directory to 0.6.
//!
//! `tests/fixtures/server-0.5.44/` holds what a 0.5.44 server left on disk,
//! written by engine 0.5.44 (`generate.rs` there): a cleanly closed database
//! (`default`), one created with options recorded in `options.json`
//! (`kept`), one whose last commits are only in its WAL, as after a crash
//! (`crashed`), one in the pre-0.5 `grafeo.db` WAL-directory layout
//! (`legacy`), and a full backup of `default` with its 0.5 (bincode)
//! manifest. Engine 0.6 migrates a 0.5.x database on its first read-write
//! open and keeps the old files under `.pre-0.6` names.

use std::path::{Path, PathBuf};

use grafeo_service::backup::BackupService;
use grafeo_service::database::DatabaseManager;
use grafeo_service::error::ServiceError;
use grafeo_service::types::{CreateDatabaseRequest, DatabaseOptions, DatabaseType, StorageMode};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/server-0.5.44")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A copy of the fixture: `(tempdir, data dir, backup dir)`.
fn upgraded_copy() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let backups = tmp.path().join("backups");
    copy_dir(&fixture().join("data"), &data);
    copy_dir(&fixture().join("backups"), &backups);
    (tmp, data, backups)
}

fn count(mgr: &DatabaseManager, db: &str, label: &str) -> i64 {
    let entry = mgr
        .get(db)
        .unwrap_or_else(|| panic!("database '{db}' is open"));
    let result = entry
        .db()
        .session()
        .execute(&format!("MATCH (n:{label}) RETURN count(n) AS c"))
        .unwrap();
    match result.rows()[0][0] {
        grafeo_common::Value::Int64(n) => n,
        ref other => panic!("count is {other:?}"),
    }
}

fn exists(path: impl AsRef<Path>) -> bool {
    path.as_ref().exists()
}

/// Every database of a 0.5.44 data directory opens with its data, its
/// last commits replayed from the WAL, and its old files kept under
/// `.pre-0.6` names; a restart opens the migrated files as they are.
#[test]
fn a_0_5_44_data_directory_migrates_at_startup() {
    let (_tmp, data, _backups) = upgraded_copy();

    let mgr = DatabaseManager::new(Some(data.to_str().unwrap()), false);
    assert_eq!(count(&mgr, "default", "Old"), 3);
    assert_eq!(count(&mgr, "kept", "Kept"), 2);
    assert_eq!(count(&mgr, "crashed", "Before"), 1);
    assert_eq!(
        count(&mgr, "crashed", "AfterCheckpoint"),
        2,
        "the commits only the 0.5 WAL held are replayed"
    );
    assert_eq!(count(&mgr, "legacy", "Legacy"), 4);

    // options.json still gives the server its database type and options.
    let kept = mgr.get("kept").unwrap();
    assert!(!kept.metadata.backward_edges);
    assert_eq!(kept.metadata.threads, 2);
    assert_eq!(kept.db().memory_limit(), Some(67_108_864));

    // The 0.5 files are kept for a way back.
    assert!(data.join("default/data.grafeo").is_file());
    assert!(exists(data.join("default/data.grafeo.pre-0.6")));
    assert!(exists(data.join("crashed/data.grafeo.pre-0.6")));
    assert!(
        exists(data.join("crashed/data.grafeo.pre-0.6.wal")),
        "the 0.5 WAL is kept with the 0.5 file"
    );
    // The legacy WAL directory became a file at the same path.
    assert!(!exists(data.join("legacy/grafeo.db")));
    assert!(data.join("legacy/data.grafeo").is_file());
    assert!(data.join("legacy/data.grafeo.pre-0.6").is_dir());

    // A write after the migration, then a restart.
    mgr.get("default")
        .unwrap()
        .db()
        .session()
        .execute("INSERT (:New)")
        .unwrap();
    for name in ["default", "kept", "crashed", "legacy"] {
        mgr.get(name).unwrap().db().close().unwrap();
    }
    drop(mgr);

    let mgr = DatabaseManager::new(Some(data.to_str().unwrap()), false);
    assert_eq!(count(&mgr, "default", "Old"), 3);
    assert_eq!(count(&mgr, "default", "New"), 1);
    assert_eq!(count(&mgr, "crashed", "AfterCheckpoint"), 2);
    assert_eq!(count(&mgr, "legacy", "Legacy"), 4);
    assert!(
        !exists(data.join("default/data.grafeo.pre-0.6.pre-0.6")),
        "a migrated file is not migrated again"
    );
}

/// A read-only server reads a 0.5.44 data directory without changing it.
#[test]
fn a_read_only_server_reads_a_0_5_44_directory_without_migrating() {
    let (_tmp, data, _backups) = upgraded_copy();

    let mgr = DatabaseManager::new(Some(data.to_str().unwrap()), true);
    assert_eq!(count(&mgr, "default", "Old"), 3);
    assert_eq!(count(&mgr, "kept", "Kept"), 2);
    assert_eq!(
        count(&mgr, "crashed", "AfterCheckpoint"),
        2,
        "a read-only open replays the WAL in memory"
    );
    drop(mgr);
    for name in ["default", "kept", "crashed"] {
        assert!(
            !exists(data.join(name).join("data.grafeo.pre-0.6")),
            "{name} was not migrated"
        );
    }
}

/// A database directory that holds only the `.pre-0.6` copy (the migrated
/// file was removed) is never adopted by a create: the copy may be the only
/// one of that database.
#[test]
fn create_refuses_a_directory_with_a_pre_0_6_copy() {
    let (_tmp, data, _backups) = upgraded_copy();
    let ghost = data.join("ghost");
    std::fs::create_dir_all(&ghost).unwrap();
    std::fs::copy(
        data.join("default/data.grafeo"),
        ghost.join("data.grafeo.pre-0.6"),
    )
    .unwrap();

    let mgr = DatabaseManager::new(Some(data.to_str().unwrap()), false);
    assert!(mgr.get("ghost").is_none());
    let err = mgr
        .create(&CreateDatabaseRequest {
            name: "ghost".to_string(),
            database_type: DatabaseType::Lpg,
            storage_mode: StorageMode::Persistent,
            options: DatabaseOptions::default(),
            schema_file: None,
            schema_filename: None,
        })
        .unwrap_err();
    assert!(matches!(err, ServiceError::Conflict(_)), "{err:?}");
    assert!(
        exists(ghost.join("data.grafeo.pre-0.6")),
        "the copy is kept"
    );
}

/// After the upgrade: a 0.5.44 backup still restores (and stays as it was),
/// new full and incremental backups work, and an epoch restore replays them.
#[tokio::test]
async fn backups_work_across_the_upgrade() {
    let (_tmp, data, backups) = upgraded_copy();
    let backup_files = |dir: &Path| {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let before = backup_files(&backups.join("default"));

    let mgr = DatabaseManager::new(Some(data.to_str().unwrap()), false);
    let listed = BackupService::list_backups(Some("default"), &backups).unwrap();
    let old = listed
        .iter()
        .find(|b| b.filename == "backup_full_0000.grafeo")
        .expect("the 0.5 manifest is read");

    // Change the database, then restore the 0.5.44 backup file.
    mgr.get("default")
        .unwrap()
        .db()
        .session()
        .execute("INSERT (:AfterUpgrade)")
        .unwrap();
    BackupService::restore_database(
        &mgr,
        "default",
        &backups.join("default").join(&old.filename),
        &backups,
    )
    .await
    .unwrap();
    assert_eq!(count(&mgr, "default", "Old"), 3);
    assert_eq!(count(&mgr, "default", "AfterUpgrade"), 0);
    let after: Vec<String> = backup_files(&backups.join("default"))
        .into_iter()
        .filter(|name| before.contains(name) || name.contains("pre-0.6"))
        .collect();
    assert_eq!(after, before, "restoring a 0.5 backup leaves it as it was");

    // New backups on the migrated database, then an epoch restore.
    let full = BackupService::backup_database(&mgr, "default", &backups, None)
        .await
        .unwrap();
    mgr.get("default")
        .unwrap()
        .db()
        .session()
        .execute("INSERT (:Incremental)")
        .unwrap();
    let incremental = BackupService::backup_incremental(&mgr, "default", &backups)
        .await
        .unwrap();
    assert!(incremental.end_epoch > full.end_epoch);
    mgr.get("default")
        .unwrap()
        .db()
        .session()
        .execute("INSERT (:Later)")
        .unwrap();

    BackupService::restore_to_epoch(&mgr, "default", incremental.end_epoch, &backups)
        .await
        .unwrap();
    assert_eq!(count(&mgr, "default", "Old"), 3);
    assert_eq!(count(&mgr, "default", "Incremental"), 1);
    assert_eq!(count(&mgr, "default", "Later"), 0);
}
