//! In-process replication correctness tests.
//!
//! Spins up primary + replica servers on ephemeral ports, manually drives
//! the pull/apply CDC cycle, and verifies data convergence.
//!
//! ```bash
//! cargo test --features "full" --test replication -- --nocapture
//! ```

#![cfg(all(feature = "http", feature = "sync"))]

use reqwest::Client;
use serde_json::{Value, json};
use std::net::SocketAddr;
use tokio::net::TcpListener;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Boots a primary in-memory server on an ephemeral port.
async fn spawn_primary() -> String {
    let state = grafeo_server::AppState::new_in_memory(300);
    spawn_server(state).await
}

/// Boots a replica in-memory server on an ephemeral port.
/// The `primary_url` is used for the replica guard middleware (write rejection),
/// but we don't start the background replication task in tests.
async fn spawn_replica(primary_url: &str) -> String {
    let config = grafeo_service::ServiceConfig {
        data_dir: None,
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
        #[cfg(feature = "replication")]
        replication_mode: grafeo_service::replication::ReplicationMode::Replica {
            primary_url: primary_url.to_string(),
        },
    };
    let service = grafeo_service::ServiceState::new(&config);
    let state = grafeo_server::AppState::new(
        service,
        vec![],
        grafeo_service::types::EnabledFeatures::default(),
    );
    spawn_server(state).await
}

async fn spawn_server(state: grafeo_server::AppState) -> String {
    let app = grafeo_server::router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
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

/// Executes a GQL query on a server and returns the JSON response body.
async fn query(client: &Client, base: &str, gql: &str) -> Value {
    client
        .post(format!("{base}/query"))
        .json(&json!({"query": gql}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// Returns the node count by querying MATCH (n) RETURN count(n).
async fn node_count(client: &Client, base: &str) -> i64 {
    let resp = query(client, base, "MATCH (n) RETURN count(n) AS cnt").await;
    // Response format: {"columns": ["cnt"], "rows": [[5]]}
    resp["rows"][0][0].as_i64().unwrap_or(0)
}

/// Pulls changes from primary and applies to replica.
/// Returns the new server epoch after applying.
async fn replicate(client: &Client, primary: &str, replica: &str, since: u64) -> u64 {
    // Pull changes from primary (since is exclusive: start from since+1)
    let fetch_since = if since > 0 { since + 1 } else { 0 };
    let changes_resp: Value = client
        .get(format!(
            "{primary}/db/default/changes?since={fetch_since}&limit=10000"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let server_epoch = changes_resp["server_epoch"].as_u64().unwrap_or(0);
    let changes = changes_resp["changes"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    if changes.is_empty() {
        return server_epoch;
    }

    // Convert ChangeEventDto to SyncChangeRequest format
    let sync_changes: Vec<Value> = changes
        .into_iter()
        .map(|dto| {
            json!({
                "kind": dto["kind"],
                "entity_type": dto["entity_type"],
                "id": dto["id"],
                "timestamp": dto["timestamp"],
                "labels": dto["labels"],
                "edge_type": dto["edge_type"],
                "src_id": dto["src_id"],
                "dst_id": dto["dst_id"],
                "after": dto["after"],
            })
        })
        .collect();

    // Apply to replica
    let apply_resp: Value = client
        .post(format!("{replica}/db/default/sync"))
        .json(&json!({
            "client_id": "test-replicator",
            "last_seen_epoch": since,
            "changes": sync_changes,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(
        apply_resp["conflicts"]
            .as_array()
            .map_or(true, |c| c.is_empty()),
        "Unexpected conflicts during replication: {:?}",
        apply_resp["conflicts"]
    );

    server_epoch
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn primary_writes_replicate_to_replica() {
    let client = Client::new();
    let primary = spawn_primary().await;
    let replica = spawn_replica(&primary).await;

    // Write 5 nodes to primary
    for i in 0..5 {
        query(
            &client,
            &primary,
            &format!("INSERT (:Person {{name: 'Person_{i}'}})"),
        )
        .await;
    }

    assert_eq!(node_count(&client, &primary).await, 5);
    assert_eq!(
        node_count(&client, &replica).await,
        0,
        "Replica should start empty"
    );

    // Replicate
    replicate(&client, &primary, &replica, 0).await;

    // Verify replica has all nodes
    let replica_count = node_count(&client, &replica).await;
    assert_eq!(
        replica_count, 5,
        "Replica should have all 5 nodes after replication, got {replica_count}"
    );
}

#[tokio::test]
async fn session_mutations_replicate() {
    let client = Client::new();
    let primary = spawn_primary().await;
    let replica = spawn_replica(&primary).await;

    // Write via GQL INSERT (session-driven, exercises CdcGraphStore)
    query(
        &client,
        &primary,
        "INSERT (:Person {name: 'Alix', city: 'Amsterdam'})",
    )
    .await;
    query(
        &client,
        &primary,
        "INSERT (:Person {name: 'Gus'})-[:KNOWS]->(:Person {name: 'Vincent'})",
    )
    .await;

    let primary_count = node_count(&client, &primary).await;
    assert!(
        primary_count >= 3,
        "Primary should have at least 3 nodes, got {primary_count}"
    );

    // Replicate
    replicate(&client, &primary, &replica, 0).await;

    let replica_count = node_count(&client, &replica).await;
    assert!(
        replica_count >= 1,
        "Replica should have replicated nodes, got {replica_count}"
    );
}

#[tokio::test]
async fn replica_rejects_mutation_via_put() {
    let client = Client::new();
    let primary = spawn_primary().await;
    let replica = spawn_replica(&primary).await;

    // PUT/DELETE requests are blocked by the replica guard middleware
    let resp = client
        .put(format!("{replica}/db/default"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status().as_u16(),
        503,
        "Replica should reject PUT with 503"
    );
}

#[tokio::test]
async fn replica_allows_read_queries() {
    let client = Client::new();
    let primary = spawn_primary().await;
    let replica = spawn_replica(&primary).await;

    // Read queries should work on replicas
    let resp = client
        .post(format!("{replica}/query"))
        .json(&json!({"query": "MATCH (n) RETURN count(n) AS cnt"}))
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status().as_u16(),
        200,
        "Replica should allow read queries"
    );
}

#[tokio::test]
async fn convergence_after_multiple_batches() {
    let client = Client::new();
    let primary = spawn_primary().await;
    let replica = spawn_replica(&primary).await;

    let mut last_epoch = 0u64;

    // Batch 1: 3 nodes
    for i in 0..3 {
        query(&client, &primary, &format!("INSERT (:Batch1 {{seq: {i}}})")).await;
    }
    last_epoch = replicate(&client, &primary, &replica, last_epoch).await;

    // Batch 2: 2 nodes
    for i in 0..2 {
        query(&client, &primary, &format!("INSERT (:Batch2 {{seq: {i}}})")).await;
    }
    last_epoch = replicate(&client, &primary, &replica, last_epoch).await;

    // Batch 3: 4 nodes
    for i in 0..4 {
        query(&client, &primary, &format!("INSERT (:Batch3 {{seq: {i}}})")).await;
    }
    let _ = replicate(&client, &primary, &replica, last_epoch).await;

    // Verify counts match
    let primary_count = node_count(&client, &primary).await;
    let replica_count = node_count(&client, &replica).await;
    assert_eq!(
        primary_count, replica_count,
        "Primary ({primary_count}) and replica ({replica_count}) should have same node count"
    );
    assert_eq!(primary_count, 9, "Should have 3 + 2 + 4 = 9 nodes");
}

#[tokio::test]
async fn concurrent_writes_all_replicate() {
    let client = Client::new();
    let primary = spawn_primary().await;
    let replica = spawn_replica(&primary).await;

    // 4 concurrent writers
    let mut handles = Vec::new();
    for tid in 0..4 {
        let client = client.clone();
        let primary = primary.clone();
        handles.push(tokio::spawn(async move {
            for i in 0..5 {
                query(
                    &client,
                    &primary,
                    &format!("INSERT (:Worker {{tid: {tid}, seq: {i}}})"),
                )
                .await;
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    let primary_count = node_count(&client, &primary).await;
    assert_eq!(
        primary_count, 20,
        "Primary should have 4*5=20 nodes, got {primary_count}"
    );

    // Replicate all at once
    replicate(&client, &primary, &replica, 0).await;

    let replica_count = node_count(&client, &replica).await;
    assert_eq!(
        replica_count, primary_count,
        "Replica should match primary after replication"
    );
}
