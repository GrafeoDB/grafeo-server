//! Writes a grafeo-server 0.5.44 data directory and backup directory with
//! engine 0.5.44, as fixtures for the 0.6 migration tests (`data/` and
//! `backups/` next to this file). Not part of any crate: to run it again, put
//! it in a binary crate as `src/main.rs` with
//!
//! ```toml
//! [dependencies]
//! grafeo-engine = { version = "=0.5.44", features = ["cypher"] }
//! grafeo-common = "=0.5.44"
//! serde_json = "1"
//! ```
//!
//! and run `cargo run --release -- <output dir>`.

use std::path::Path;

use grafeo_engine::{Config, DurabilityMode, GrafeoDB};

fn insert(db: &GrafeoDB, label: &str, n: i64) {
    let session = db.session();
    for i in 0..n {
        session
            .execute(&format!("INSERT (:{label} {{i: {i}}})"))
            .unwrap();
    }
}

fn main() {
    let out = Path::new(&std::env::args().nth(1).expect("output dir")).to_path_buf();
    let data = out.join("data");
    let backups = out.join("backups");
    std::fs::create_dir_all(&data).unwrap();

    // default: a single-file database closed cleanly.
    let dir = data.join("default");
    std::fs::create_dir_all(&dir).unwrap();
    let db = GrafeoDB::open(dir.join("data.grafeo")).unwrap();
    insert(&db, "Old", 3);
    // A full backup with the 0.5 (bincode) manifest.
    std::fs::create_dir_all(backups.join("default")).unwrap();
    let seg = db.backup_full(&backups.join("default")).unwrap();
    println!("backup {}", seg.filename);
    db.close().unwrap();

    // kept: created by the server with options, recorded in options.json.
    let dir = data.join("kept");
    std::fs::create_dir_all(&dir).unwrap();
    let options = serde_json::json!({
        "database_type": "Lpg",
        "options": {
            "memory_limit_bytes": 67_108_864,
            "backward_edges": false,
            "threads": 2,
            "wal_enabled": null,
            "wal_durability": null,
            "spill_path": null,
            "section_tiers": null
        }
    });
    std::fs::write(
        dir.join("options.json"),
        serde_json::to_string_pretty(&options).unwrap(),
    )
    .unwrap();
    let db = GrafeoDB::with_config(
        Config::persistent(dir.join("data.grafeo"))
            .with_memory_limit(67_108_864)
            .with_threads(2)
            .without_backward_edges(),
    )
    .unwrap();
    insert(&db, "Kept", 2);
    db.close().unwrap();

    // crashed: commits after the last checkpoint are only in the WAL, as
    // after a crash (the handle is leaked, never closed).
    let dir = data.join("crashed");
    std::fs::create_dir_all(&dir).unwrap();
    let db = GrafeoDB::with_config(
        Config::persistent(dir.join("data.grafeo")).with_wal_durability(DurabilityMode::Sync),
    )
    .unwrap();
    insert(&db, "Before", 1);
    db.close().unwrap();
    let db = GrafeoDB::with_config(
        Config::persistent(dir.join("data.grafeo")).with_wal_durability(DurabilityMode::Sync),
    )
    .unwrap();
    insert(&db, "AfterCheckpoint", 2);
    std::mem::forget(db);

    // legacy: the pre-0.5 server layout, a WAL-directory database named
    // grafeo.db that the server's startup renames to data.grafeo.
    let dir = data.join("legacy");
    std::fs::create_dir_all(&dir).unwrap();
    let db = GrafeoDB::open(dir.join("grafeo.db")).unwrap();
    insert(&db, "Legacy", 4);
    db.close().unwrap();
    println!("done");
}
