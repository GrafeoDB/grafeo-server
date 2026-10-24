//! A query write after a compaction survives a crash (grafeo#558: engine
//! 0.5.x did not write it to the WAL, so a crash before the next checkpoint
//! lost it). The crash is a child process, this test binary run again, that
//! compacts a persistent database through the server's admin service, writes,
//! and exits without closing anything.

use grafeo_service::admin::AdminService;
use grafeo_service::database::DatabaseManager;
use grafeo_service::types::{CreateDatabaseRequest, DatabaseOptions, DatabaseType, StorageMode};

/// Set in the child: the data directory to write to.
const CHILD_DATA_DIR: &str = "GRAFEO_TEST_CRASH_AFTER_COMPACT_DIR";

fn service(data_dir: &str) -> grafeo_service::ServiceState {
    grafeo_service::ServiceState::new(&grafeo_service::ServiceConfig {
        data_dir: Some(data_dir.to_string()),
        read_only: false,
        session_ttl: 300,
        query_timeout: 30,
        rate_limit: 0,
        rate_limit_window: 60,
        #[cfg(feature = "auth")]
        auth_token: None,
        #[cfg(feature = "auth")]
        auth_user: None,
        #[cfg(feature = "auth")]
        auth_password: None,
        #[cfg(feature = "auth")]
        token_store_path: None,
        #[cfg(feature = "replication")]
        replication_mode: grafeo_service::replication::ReplicationMode::Standalone,
        backup_dir: None,
        backup_retention: None,
    })
}

/// The child's part; a no-op in a normal test run.
#[tokio::test]
async fn crash_child() {
    let Ok(data_dir) = std::env::var(CHILD_DATA_DIR) else {
        return;
    };
    let state = service(&data_dir);
    state
        .databases()
        .create(&CreateDatabaseRequest {
            name: "compacted".to_string(),
            database_type: DatabaseType::Lpg,
            storage_mode: StorageMode::Persistent,
            options: DatabaseOptions {
                wal_durability: Some("sync".to_string()),
                ..DatabaseOptions::default()
            },
            schema_file: None,
            schema_filename: None,
        })
        .unwrap();
    let run = |q: &str| {
        state
            .databases()
            .get("compacted")
            .unwrap()
            .db()
            .session()
            .execute(q)
            .unwrap();
    };
    run("INSERT (:BeforeCompact)");
    let report = AdminService::compact(&state, "compacted").await.unwrap();
    assert!(report.checkpointed);
    run("INSERT (:AfterCompact)");
    // The crash: no close, no destructors.
    std::process::exit(0);
}

#[test]
fn a_write_after_compact_survives_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child", "--nocapture", "--test-threads=1"])
        .env(CHILD_DATA_DIR, dir.path())
        .status()
        .unwrap();
    assert!(status.success(), "the child failed: {status}");

    let mgr = DatabaseManager::new(dir.path().to_str(), false);
    let entry = mgr.get("compacted").expect("the database opens");
    let result = entry
        .db()
        .session()
        .execute("MATCH (n) RETURN labels(n)[0] AS l ORDER BY l")
        .unwrap();
    let labels: Vec<String> = result
        .rows()
        .iter()
        .map(|row| match &row[0] {
            grafeo_common::Value::String(s) => s.to_string(),
            other => panic!("label is {other:?}"),
        })
        .collect();
    assert_eq!(labels, ["AfterCompact", "BeforeCompact"]);
}
