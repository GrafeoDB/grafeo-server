//! Background replication task for replica instances.
//!
//! When the server starts in `Replica` mode, `start` spawns a Tokio task
//! for each database that:
//!
//! 1. Calls `GET /db/{name}/changes?since={last_epoch + 1}&limit=500` on the
//!    primary (`since=0` before the first batch).
//! 2. Applies the returned events via `SyncService::apply()`.
//! 3. Advances the local epoch counter in `ReplicationState` to the
//!    response's `server_epoch`, the primary's resume cursor. The primary
//!    never splits an epoch, so a burst larger than the batch limit arrives
//!    over several polls without a gap.
//! 4. Sleeps `POLL_INTERVAL` before the next iteration.
//!
//! The task runs indefinitely until the process exits.  Transient HTTP errors
//! are logged and retried; the error is recorded in `ReplicationState`.
//! Changes of a batch that cannot be applied are logged at `warn` and
//! summarised in `ReplicationState` (see `ReplicationState::set_conflict`);
//! the epoch still advances, since replaying the batch would duplicate its
//! creates.

use std::sync::Arc;
use std::time::Duration;

use grafeo_service::ServiceState;
use grafeo_service::error::ServiceError;
use grafeo_service::replication::ReplicationState;
use grafeo_service::sync::{ConflictRecord, SyncChangeRequest, SyncRequest, SyncService};
use tracing::{debug, error, info, warn};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const BATCH_LIMIT: usize = 500;
/// Conflicts of one batch logged one by one; the rest are counted.
const MAX_LOGGED_CONFLICTS: usize = 20;

/// Starts background replication tasks for all databases on a replica.
///
/// This is a no-op when not in `Replica` mode.
pub fn start(state: ServiceState) {
    if !state.is_replica() {
        return;
    }

    let primary_url = match state.replication_mode().primary_url() {
        Some(url) => url.to_string(),
        None => return,
    };

    info!(
        primary_url = %primary_url,
        "Starting replication background task"
    );

    let replication_state = Arc::clone(state.replication_state());

    tokio::spawn(async move {
        poll_loop(state, primary_url, replication_state).await;
        error!("Replication poll loop exited unexpectedly");
    });
}

/// The main polling loop. Polls all known databases on the primary.
async fn poll_loop(
    state: ServiceState,
    primary_url: String,
    replication_state: Arc<ReplicationState>,
) {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest client construction cannot fail");

    loop {
        // Collect all database names known locally.
        let db_names: Vec<String> = state
            .databases()
            .list()
            .into_iter()
            .map(|info| info.name)
            .collect();

        // Also ensure "default" is always replicated even if not yet created.
        let mut names = db_names;
        if !names.iter().any(|n| n == "default") {
            names.push("default".to_string());
        }

        for db_name in &names {
            poll_database(&http, &state, &primary_url, db_name, &replication_state).await;
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Replicates one batch for `db_name` and records how the poll went: its
/// error, or no error once a poll succeeds. A conflict summary recorded by
/// the batch stays (see `ReplicationState::set_conflict`).
async fn poll_database(
    http: &reqwest::Client,
    state: &ServiceState,
    primary_url: &str,
    db_name: &str,
    replication_state: &ReplicationState,
) {
    if let Err(e) = replicate_database(http, state, primary_url, db_name, replication_state).await {
        warn!(db = %db_name, error = %e, "Replication poll failed");
        replication_state.set_error(db_name, e.to_string());
    } else {
        replication_state.clear_error(db_name);
    }
}

/// Fetches and applies one batch of changes for a single database.
async fn replicate_database(
    http: &reqwest::Client,
    state: &ServiceState,
    primary_url: &str,
    db_name: &str,
    replication_state: &ReplicationState,
) -> Result<(), ReplicationError> {
    let last = replication_state.last_epoch(db_name);
    // `last` is the primary's resume cursor (`server_epoch`) of the last
    // batch applied: every event up to it is here, so continue after it.
    let since = if last > 0 { last + 1 } else { 0 };
    let url = format!("{primary_url}/db/{db_name}/changes?since={since}&limit={BATCH_LIMIT}");

    debug!(db = %db_name, since = since, "Polling primary for changes");

    let resp = http
        .get(&url)
        .send()
        .await
        .map_err(ReplicationError::Http)?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        // Database doesn't exist on the primary yet — skip silently.
        return Ok(());
    }

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(ReplicationError::BadStatus { status, body });
    }

    let changes_resp: grafeo_service::sync::ChangesResponse =
        resp.json().await.map_err(ReplicationError::Http)?;

    if changes_resp.changes.is_empty() {
        return Ok(());
    }

    let new_epoch = changes_resp.server_epoch;
    let count = changes_resp.changes.len();

    info!(
        db = %db_name,
        changes = count,
        up_to_epoch = new_epoch,
        "Applying replication batch"
    );

    // Ensure the database exists on the replica.
    if state.databases().get(db_name).is_none() {
        let create_req = grafeo_service::types::CreateDatabaseRequest {
            name: db_name.to_string(),
            database_type: grafeo_service::types::DatabaseType::default(),
            storage_mode: grafeo_service::types::StorageMode::default(),
            options: grafeo_service::types::DatabaseOptions::default(),
            schema_file: None,
            schema_filename: None,
        };
        if let Err(e) = state.databases().create(&create_req) {
            return Err(ReplicationError::Apply(format!(
                "Failed to create local database '{db_name}': {e}"
            )));
        }
    }

    // Convert ChangeEventDto → SyncChangeRequest and apply.
    let sync_changes: Vec<SyncChangeRequest> = changes_resp
        .changes
        .iter()
        .map(change_event_to_sync_request)
        .collect();

    let req = SyncRequest {
        client_id: "replication".to_string(),
        last_seen_epoch: since,
        changes: sync_changes,
        schema_version: None,
    };

    let state_clone = state.clone();
    let db_name_owned = db_name.to_string();
    let result = tokio::task::spawn_blocking(move || {
        SyncService::apply(state_clone.databases(), &db_name_owned, req)
    })
    .await
    .unwrap_or_else(|e| {
        error!(error = %e, "SyncService::apply panicked");
        Err(ServiceError::Internal(format!("apply panicked: {e}")))
    });

    match result {
        Ok(resp) => {
            if !resp.conflicts.is_empty() {
                report_conflicts(db_name, new_epoch, &resp.conflicts, replication_state);
            }
            if resp.applied > 0 || resp.skipped > 0 {
                debug!(
                    db = %db_name,
                    applied = resp.applied,
                    skipped = resp.skipped,
                    "Applied replication batch"
                );
            }
        }
        Err(e) => {
            return Err(ReplicationError::Apply(e.to_string()));
        }
    }

    replication_state.advance_epoch(db_name, new_epoch);
    Ok(())
}

/// Logs the changes of the batch up to `up_to_epoch` that could not be
/// applied (the first `MAX_LOGGED_CONFLICTS`, then a count) and records a
/// summary for the replication status.
fn report_conflicts(
    db_name: &str,
    up_to_epoch: u64,
    conflicts: &[ConflictRecord],
    replication_state: &ReplicationState,
) {
    for conflict in conflicts.iter().take(MAX_LOGGED_CONFLICTS) {
        warn!(
            db = %db_name,
            up_to_epoch,
            request_index = conflict.request_index,
            reason = %conflict.reason,
            "Replicated change could not be applied"
        );
    }
    if conflicts.len() > MAX_LOGGED_CONFLICTS {
        warn!(
            db = %db_name,
            up_to_epoch,
            more = conflicts.len() - MAX_LOGGED_CONFLICTS,
            "More replicated changes could not be applied"
        );
    }
    replication_state.set_conflict(db_name, conflict_summary(conflicts, up_to_epoch));
}

/// One line on the changes of a batch that could not be applied.
fn conflict_summary(conflicts: &[ConflictRecord], up_to_epoch: u64) -> String {
    let first = conflicts.first().map_or("", |c| c.reason.as_str());
    format!(
        "{} change(s) could not be applied in the batch up to epoch {up_to_epoch}; first: {first}",
        conflicts.len()
    )
}

/// Converts a `ChangeEventDto` to a `SyncChangeRequest` for replay.
fn change_event_to_sync_request(event: &grafeo_service::sync::ChangeEventDto) -> SyncChangeRequest {
    let after = event.after.clone();
    SyncChangeRequest {
        kind: event.kind.clone(),
        entity_type: event.entity_type.clone(),
        id: Some(event.id),
        timestamp: event.timestamp,
        labels: event.labels.clone(),
        edge_type: event.edge_type.clone(),
        src_id: event.src_id,
        dst_id: event.dst_id,
        after,
        crdt_op: None,
        crdt_property: None,
        graph: event.graph.clone(),
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum ReplicationError {
    Http(reqwest::Error),
    BadStatus { status: u16, body: String },
    Apply(String),
}

impl std::fmt::Display for ReplicationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(e) => write!(f, "HTTP error: {e}"),
            Self::BadStatus { status, body } => {
                write!(f, "Primary returned {status}: {body}")
            }
            Self::Apply(e) => write!(f, "Apply error: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::*;

    /// Serves `state` over HTTP on an ephemeral port; returns its base URL.
    async fn serve(state: ServiceState) -> String {
        let app = crate::router(crate::AppState::new(
            state,
            vec![],
            grafeo_service::types::EnabledFeatures::default(),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        format!("http://{addr}")
    }

    /// An in-memory primary whose default database records CDC.
    fn primary() -> ServiceState {
        let state = ServiceState::new_in_memory(300);
        default_db(&state).set_cdc_enabled(true);
        state
    }

    fn default_db(state: &ServiceState) -> std::sync::Arc<grafeo_engine::GrafeoDB> {
        state.databases().get("default").unwrap().db()
    }

    /// Creates `n` nodes in one batch: one transaction, one epoch.
    fn insert_nodes(state: &ServiceState, label: &str, n: usize) {
        let rows = (0..n)
            .map(|i| {
                std::collections::HashMap::from([(
                    grafeo_common::types::PropertyKey::new("i"),
                    grafeo_common::Value::Int64(i as i64),
                )])
            })
            .collect();
        default_db(state)
            .batch_create_nodes_with_props(label, rows)
            .unwrap();
    }

    #[tokio::test]
    async fn replicates_a_burst_larger_than_the_batch_limit() {
        let primary = primary();
        // One transaction larger than a batch, then three a batch must cut
        // between.
        insert_nodes(&primary, "Burst", BATCH_LIMIT + 150);
        for label in ["A", "B", "C"] {
            insert_nodes(&primary, label, 300);
        }
        let url = serve(primary.clone()).await;

        let replica = ServiceState::new_in_memory(300);
        let progress = ReplicationState::new();
        let http = reqwest::Client::new();
        let mut batches = 0;
        loop {
            let before = progress.last_epoch("default");
            replicate_database(&http, &replica, &url, "default", &progress)
                .await
                .unwrap();
            if progress.last_epoch("default") == before {
                break;
            }
            batches += 1;
            assert!(batches < 10, "replication does not catch up");
        }

        assert_eq!(batches, 3, "650, then 300 + 300, then 300");
        assert_eq!(default_db(&replica).node_count(), BATCH_LIMIT + 150 + 900);
        assert_eq!(
            progress.last_epoch("default"),
            default_db(&primary).current_epoch().0
        );
    }

    fn last_error(progress: &ReplicationState) -> Option<String> {
        progress
            .status(&grafeo_service::replication::ReplicationMode::Standalone)
            .databases["default"]
            .last_error
            .clone()
    }

    #[tokio::test]
    async fn apply_conflicts_are_reported_and_stay_visible() {
        let primary = primary();
        let db = default_db(&primary);
        let alix = db.create_node(&["Person"]).unwrap();
        let gus = db.create_node(&["Person"]).unwrap();
        let nodes_epoch = db.current_epoch().0;
        db.create_edge(alix, gus, "KNOWS").unwrap();
        let edge_epoch = db.current_epoch().0;
        let url = serve(primary.clone()).await;

        // The replica starts after the nodes, so the edge's endpoints are
        // missing there and its create cannot be applied.
        let replica = ServiceState::new_in_memory(300);
        let progress = ReplicationState::new();
        progress.advance_epoch("default", nodes_epoch);
        let http = reqwest::Client::new();
        poll_database(&http, &replica, &url, "default", &progress).await;

        assert_eq!(progress.last_epoch("default"), edge_epoch, "still advances");
        assert_eq!(default_db(&replica).edge_count(), 0);
        let error = last_error(&progress).expect("the conflict is reported");
        let expected = format!(
            "1 change(s) could not be applied in the batch up to epoch {edge_epoch}; first: write_failed:"
        );
        assert!(error.starts_with(&expected), "{error}");

        // A later clean poll does not wipe it.
        poll_database(&http, &replica, &url, "default", &progress).await;
        assert_eq!(last_error(&progress), Some(error));
    }

    #[test]
    fn conflict_summary_counts_and_names_the_first() {
        let conflicts = vec![
            ConflictRecord {
                request_index: 4,
                reason: "write_failed: node 9 not found".to_string(),
            },
            ConflictRecord {
                request_index: 7,
                reason: "server_newer".to_string(),
            },
        ];
        assert_eq!(
            conflict_summary(&conflicts, 12),
            "2 change(s) could not be applied in the batch up to epoch 12; first: write_failed: node 9 not found"
        );
    }
}
