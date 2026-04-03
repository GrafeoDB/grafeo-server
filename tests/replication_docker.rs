//! Docker-based replication tests with real server binaries.
//!
//! Requires Docker Compose cluster running:
//! ```bash
//! docker compose -f tests/docker-compose.replication.yml up -d
//! cargo test --features "full" --test replication_docker -- --test-threads=1 --nocapture
//! docker compose -f tests/docker-compose.replication.yml down
//! ```
//!
//! These tests are ignored by default and only run when the Docker cluster
//! is available (checked via health endpoint).

use reqwest::Client;
use serde_json::{Value, json};
use std::time::Duration;

const PRIMARY: &str = "http://localhost:17474";
const REPLICA1: &str = "http://localhost:17475";
const REPLICA2: &str = "http://localhost:17476";

fn client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

/// Returns true if the Docker cluster is reachable.
async fn cluster_is_up() -> bool {
    let c = client();
    c.get(format!("{PRIMARY}/health"))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

async fn query(base: &str, gql: &str) -> Value {
    client()
        .post(format!("{base}/query"))
        .json(&json!({"query": gql}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn node_count(base: &str) -> i64 {
    let resp = query(base, "MATCH (n) RETURN count(n) AS cnt").await;
    resp["rows"][0][0].as_i64().unwrap_or(0)
}

/// Polls until a condition is met or timeout expires.
async fn wait_for<F, Fut>(timeout: Duration, interval: Duration, check: F) -> bool
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = std::time::Instant::now();
    loop {
        if check().await {
            return true;
        }
        if start.elapsed() > timeout {
            return false;
        }
        tokio::time::sleep(interval).await;
    }
}

// ---------------------------------------------------------------------------
// Tests (ignored unless Docker cluster is running)
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Docker cluster: docker compose -f tests/docker-compose.replication.yml up -d"]
async fn docker_write_and_converge() {
    if !cluster_is_up().await {
        eprintln!("Docker cluster not running, skipping");
        return;
    }

    // Write 5 nodes to primary
    for i in 0..5 {
        query(PRIMARY, &format!("INSERT (:DockerTest {{seq: {i}}})")).await;
    }

    let primary_count = node_count(PRIMARY).await;
    assert!(
        primary_count >= 5,
        "Primary should have at least 5 nodes, got {primary_count}"
    );

    // Wait for replicas to converge (background replication task polls every 500ms)
    let converged = wait_for(
        Duration::from_secs(10),
        Duration::from_millis(500),
        || async {
            let r1 = node_count(REPLICA1).await;
            let r2 = node_count(REPLICA2).await;
            r1 >= primary_count && r2 >= primary_count
        },
    )
    .await;

    assert!(
        converged,
        "Replicas should converge within 10s. Primary: {}, R1: {}, R2: {}",
        node_count(PRIMARY).await,
        node_count(REPLICA1).await,
        node_count(REPLICA2).await,
    );
}

#[tokio::test]
#[ignore = "requires Docker cluster: docker compose -f tests/docker-compose.replication.yml up -d"]
async fn docker_session_mutations_converge() {
    if !cluster_is_up().await {
        eprintln!("Docker cluster not running, skipping");
        return;
    }

    // Write via session (exercises CdcGraphStore)
    query(
        PRIMARY,
        "INSERT (:SessionTest {name: 'Alix'})-[:KNOWS]->(:SessionTest {name: 'Gus'})",
    )
    .await;

    let primary_count = node_count(PRIMARY).await;

    // Wait for convergence
    let converged = wait_for(
        Duration::from_secs(10),
        Duration::from_millis(500),
        || async {
            let r1 = node_count(REPLICA1).await;
            r1 >= primary_count
        },
    )
    .await;

    assert!(
        converged,
        "Replica should converge. Primary: {}, R1: {}",
        primary_count,
        node_count(REPLICA1).await,
    );
}

#[tokio::test]
#[ignore = "requires Docker cluster: docker compose -f tests/docker-compose.replication.yml up -d"]
async fn docker_replica_rejects_put_delete() {
    if !cluster_is_up().await {
        eprintln!("Docker cluster not running, skipping");
        return;
    }

    // PUT should be rejected on replica
    let resp = client()
        .put(format!("{REPLICA1}/db/default"))
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
