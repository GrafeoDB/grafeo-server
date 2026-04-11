#![cfg(feature = "http")]
//! Integration tests for the Grafeo Server HTTP API.
//!
//! Each test starts an in-memory server on an ephemeral port and uses reqwest
//! to exercise the endpoints.

use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde_json::{Value, json};
use std::net::SocketAddr;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite;

/// Boots an in-memory Grafeo server on an OS-assigned port.
/// Returns the base URL (e.g. "http://127.0.0.1:12345").
async fn spawn_server() -> String {
    spawn_server_from_state(grafeo_server::AppState::new_in_memory(300)).await
}

/// Boots a server from a pre-configured `AppState`.
///
/// Use this when you need to seed the database before the server starts
/// (e.g. to populate the CDC log via the direct API before testing sync).
async fn spawn_server_from_state(state: grafeo_server::AppState) -> String {
    let mut app = grafeo_server::router(state);

    #[cfg(feature = "studio")]
    {
        app = grafeo_studio::router().merge(app);
    }

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

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_returns_ok() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["persistent"], false);
    assert!(body["uptime_seconds"].is_u64());
    assert!(body["active_sessions"].is_u64());
}

#[tokio::test]
async fn request_id_generated_when_absent() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let request_id = resp
        .headers()
        .get("x-request-id")
        .expect("missing x-request-id");
    // Should be a valid UUID v4
    let id_str = request_id.to_str().unwrap();
    assert_eq!(id_str.len(), 36); // UUID format: 8-4-4-4-12
}

#[tokio::test]
async fn request_id_preserved_when_provided() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/health"))
        .header("x-request-id", "my-custom-id-123")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let request_id = resp
        .headers()
        .get("x-request-id")
        .expect("missing x-request-id");
    assert_eq!(request_id.to_str().unwrap(), "my-custom-id-123");
}

// ---------------------------------------------------------------------------
// Auto-commit queries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_create_and_match() {
    let base = spawn_server().await;
    let client = Client::new();

    // Create a node
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "CREATE (n:Person {name: 'Alice'}) RETURN n.name"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert!(!body["columns"].as_array().unwrap().is_empty());

    // Match it back
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n:Person) RETURN n.name"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    let rows = body["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], "Alice");
}

#[tokio::test]
async fn query_bad_syntax_returns_400() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "NOT VALID SYNTAX %%%"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "bad_request");
}

// ---------------------------------------------------------------------------
// Cypher convenience endpoint
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cypher_endpoint_works() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/cypher"))
        .json(&json!({"query": "CREATE (n:Movie {title: 'The Matrix'}) RETURN n.title"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    let rows = body["rows"].as_array().unwrap();
    assert!(!rows.is_empty());
}

// ---------------------------------------------------------------------------
// Transaction lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transaction_commit() {
    let base = spawn_server().await;
    let client = Client::new();

    // Begin transaction
    let resp = client
        .post(format!("{base}/tx/begin"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "open");
    let session_id = body["session_id"].as_str().unwrap().to_string();

    // Create node within transaction
    let resp = client
        .post(format!("{base}/tx/query"))
        .header("X-Session-Id", &session_id)
        .json(&json!({"query": "CREATE (n:TxTest {val: 1}) RETURN n.val"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Commit
    let resp = client
        .post(format!("{base}/tx/commit"))
        .header("X-Session-Id", &session_id)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "committed");

    // Verify committed data is visible via auto-commit query
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n:TxTest) RETURN n.val"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    let rows = body["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
}

#[tokio::test]
async fn transaction_rollback() {
    let base = spawn_server().await;
    let client = Client::new();

    // Begin
    let resp = client
        .post(format!("{base}/tx/begin"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let session_id = body["session_id"].as_str().unwrap().to_string();

    // Create node
    client
        .post(format!("{base}/tx/query"))
        .header("X-Session-Id", &session_id)
        .json(&json!({"query": "CREATE (n:RollbackTest {val: 99}) RETURN n.val"}))
        .send()
        .await
        .unwrap();

    // Rollback
    let resp = client
        .post(format!("{base}/tx/rollback"))
        .header("X-Session-Id", &session_id)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "rolled_back");

    // Verify data was NOT persisted
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n:RollbackTest) RETURN n.val"}))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let rows = body["rows"].as_array().unwrap();
    assert!(rows.is_empty());
}

// ---------------------------------------------------------------------------
// Transaction error cases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tx_query_without_session_header_returns_400() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/tx/query"))
        .json(&json!({"query": "MATCH (n) RETURN count(n)"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn tx_query_with_invalid_session_returns_404() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/tx/query"))
        .header("X-Session-Id", "nonexistent-id")
        .json(&json!({"query": "MATCH (n) RETURN count(n)"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn tx_commit_after_remove_returns_404() {
    let base = spawn_server().await;
    let client = Client::new();

    // Begin
    let resp = client
        .post(format!("{base}/tx/begin"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let session_id = body["session_id"].as_str().unwrap().to_string();

    // Commit once
    client
        .post(format!("{base}/tx/commit"))
        .header("X-Session-Id", &session_id)
        .send()
        .await
        .unwrap();

    // Commit again - session already removed
    let resp = client
        .post(format!("{base}/tx/commit"))
        .header("X-Session-Id", &session_id)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// ---------------------------------------------------------------------------
// Language convenience endpoints
// ---------------------------------------------------------------------------

#[tokio::test]
async fn gremlin_endpoint_works() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/gremlin"))
        .json(&json!({"query": "g.addV('Language').property('name', 'Gremlin')"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert!(body["columns"].as_array().is_some());
}

#[tokio::test]
async fn sparql_endpoint_works() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/sparql"))
        .json(&json!({"query": "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert!(body["columns"].as_array().is_some());
}

// ---------------------------------------------------------------------------
// OpenAPI / Swagger UI
// ---------------------------------------------------------------------------

#[tokio::test]
async fn openapi_json_returns_valid_spec() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/api/openapi.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    // OpenAPI 3.1.x spec
    assert!(body["openapi"].as_str().unwrap().starts_with("3.1"));
    assert_eq!(body["info"]["title"], "Grafeo Server API");
    assert_eq!(body["info"]["version"], env!("CARGO_PKG_VERSION"));

    // Check that all expected paths are present
    let paths = body["paths"].as_object().unwrap();
    assert!(paths.contains_key("/query"));
    assert!(paths.contains_key("/cypher"));
    assert!(paths.contains_key("/graphql"));
    assert!(paths.contains_key("/gremlin"));
    assert!(paths.contains_key("/sparql"));
    assert!(paths.contains_key("/health"));
    assert!(paths.contains_key("/tx/begin"));
    assert!(paths.contains_key("/tx/query"));
    assert!(paths.contains_key("/tx/commit"));
    assert!(paths.contains_key("/tx/rollback"));
    assert!(paths.contains_key("/db"));
    assert!(paths.contains_key("/db/{name}"));
    assert!(paths.contains_key("/db/{name}/stats"));
    assert!(paths.contains_key("/db/{name}/schema"));
}

#[tokio::test]
async fn swagger_ui_serves_html() {
    let base = spawn_server().await;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .unwrap();

    let resp = client
        .get(format!("{base}/api/docs/"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let content_type = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(content_type.contains("text/html"));
}

// ---------------------------------------------------------------------------
// Example query validation
// ---------------------------------------------------------------------------
// These tests ensure that example queries shown in the README and Sidebar
// actually work against the engine. If the engine's query syntax changes,
// these tests will catch it.

#[tokio::test]
async fn readme_examples_gql() {
    let base = spawn_server().await;
    let client = Client::new();

    // GQL INSERT
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "INSERT (:Person {name: 'Alice', age: 30})"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // GQL MATCH
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (p:Person) RETURN p.name, p.age"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["rows"][0][0], "Alice");
    assert_eq!(body["rows"][0][1], 30);
}

#[tokio::test]
async fn readme_examples_cypher() {
    let base = spawn_server().await;
    let client = Client::new();

    // Seed data
    client
        .post(format!("{base}/query"))
        .json(&json!({"query": "INSERT (:Person {name: 'Test'})"}))
        .send()
        .await
        .unwrap();

    // Cypher count
    let resp = client
        .post(format!("{base}/cypher"))
        .json(&json!({"query": "MATCH (n) RETURN count(n)"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(!body["rows"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn readme_examples_graphql() {
    let base = spawn_server().await;
    let client = Client::new();

    // Seed data
    client
        .post(format!("{base}/query"))
        .json(&json!({"query": "INSERT (:Person {name: 'Alice', age: 30})"}))
        .send()
        .await
        .unwrap();

    // GraphQL
    let resp = client
        .post(format!("{base}/graphql"))
        .json(&json!({"query": "{ Person { name age } }"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["rows"][0][0], "Alice");
}

#[tokio::test]
async fn readme_examples_gremlin() {
    let base = spawn_server().await;
    let client = Client::new();

    // Seed data
    client
        .post(format!("{base}/query"))
        .json(&json!({"query": "INSERT (:Person {name: 'Alice'})"}))
        .send()
        .await
        .unwrap();

    // Gremlin
    let resp = client
        .post(format!("{base}/gremlin"))
        .json(&json!({"query": "g.V().hasLabel('Person').values('name')"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["rows"][0][0], "Alice");
}

#[tokio::test]
async fn readme_examples_sparql() {
    let base = spawn_server().await;
    let client = Client::new();

    // SPARQL INSERT DATA (RDF triple store, separate from property graph)
    let resp = client
        .post(format!("{base}/sparql"))
        .json(&json!({"query": "PREFIX foaf: <http://xmlns.com/foaf/0.1/> PREFIX ex: <http://example.org/> INSERT DATA { ex:alice a foaf:Person . ex:alice foaf:name \"Alice\" }"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // SPARQL SELECT
    let resp = client
        .post(format!("{base}/sparql"))
        .json(&json!({"query": "PREFIX foaf: <http://xmlns.com/foaf/0.1/> SELECT ?name WHERE { ?p a foaf:Person . ?p foaf:name ?name }"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["rows"][0][0], "Alice");
}

#[tokio::test]
async fn sidebar_examples() {
    let base = spawn_server().await;
    let client = Client::new();

    // Seed data
    client
        .post(format!("{base}/query"))
        .json(&json!({"query": "INSERT (:Person {name: 'Alice', age: 30})"}))
        .send()
        .await
        .unwrap();

    // "All nodes": MATCH (n) RETURN n LIMIT 25
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n) RETURN n LIMIT 25"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(!body["rows"].as_array().unwrap().is_empty());

    // "Count nodes": MATCH (n) RETURN count(n)
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n) RETURN count(n)"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["rows"][0][0].as_i64().unwrap() >= 1);

    // "Node labels": MATCH (n) RETURN DISTINCT labels(n)
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n) RETURN DISTINCT labels(n)"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // "Find by name": MATCH (p:Person {name: 'Alice'}) RETURN p
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (p:Person {name: 'Alice'}) RETURN p"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(!body["rows"].as_array().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// UI redirect
// ---------------------------------------------------------------------------

#[tokio::test]
async fn root_redirects_to_studio() {
    let base = spawn_server().await;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    let resp = client.get(&base).send().await.unwrap();
    assert_eq!(resp.status(), 308); // Permanent redirect
    assert_eq!(resp.headers().get("location").unwrap(), "/studio/");
}

// ---------------------------------------------------------------------------
// Database management
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_databases_returns_default() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client.get(format!("{base}/db")).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    let dbs = body["databases"].as_array().unwrap();
    assert_eq!(dbs.len(), 1);
    assert_eq!(dbs[0]["name"], "default");
}

#[tokio::test]
async fn create_and_delete_database() {
    let base = spawn_server().await;
    let client = Client::new();

    // Create
    let resp = client
        .post(format!("{base}/db"))
        .json(&json!({"name": "testdb"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "testdb");
    assert_eq!(body["node_count"], 0);

    // List should show 2 databases
    let resp = client.get(format!("{base}/db")).send().await.unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["databases"].as_array().unwrap().len(), 2);

    // Delete
    let resp = client
        .delete(format!("{base}/db/testdb"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // List should show 1 database
    let resp = client.get(format!("{base}/db")).send().await.unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["databases"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn cannot_delete_default_database() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .delete(format!("{base}/db/default"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn create_duplicate_database_returns_409() {
    let base = spawn_server().await;
    let client = Client::new();

    client
        .post(format!("{base}/db"))
        .json(&json!({"name": "dup"}))
        .send()
        .await
        .unwrap();

    let resp = client
        .post(format!("{base}/db"))
        .json(&json!({"name": "dup"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
}

#[tokio::test]
async fn query_on_specific_database() {
    let base = spawn_server().await;
    let client = Client::new();

    // Create a second database
    client
        .post(format!("{base}/db"))
        .json(&json!({"name": "other"}))
        .send()
        .await
        .unwrap();

    // Insert into "other" database
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "INSERT (:Widget {name: 'Gear'})", "database": "other"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Query "other" database
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (w:Widget) RETURN w.name", "database": "other"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["rows"][0][0], "Gear");

    // Default database should NOT have the widget
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (w:Widget) RETURN w.name"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["rows"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn database_info_stats_schema() {
    let base = spawn_server().await;
    let client = Client::new();

    // Seed some data
    client
        .post(format!("{base}/query"))
        .json(&json!({"query": "INSERT (:Person {name: 'Alice'})"}))
        .send()
        .await
        .unwrap();

    // Info
    let resp = client
        .get(format!("{base}/db/default"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "default");
    assert!(body["node_count"].as_u64().unwrap() >= 1);

    // Stats
    let resp = client
        .get(format!("{base}/db/default/stats"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["memory_bytes"].is_u64());

    // Schema
    let resp = client
        .get(format!("{base}/db/default/schema"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["labels"].as_array().is_some());
}

#[tokio::test]
async fn transaction_on_specific_database() {
    let base = spawn_server().await;
    let client = Client::new();

    // Create a second database
    client
        .post(format!("{base}/db"))
        .json(&json!({"name": "txdb"}))
        .send()
        .await
        .unwrap();

    // Begin transaction on "txdb"
    let resp = client
        .post(format!("{base}/tx/begin"))
        .json(&json!({"database": "txdb"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let session_id = body["session_id"].as_str().unwrap().to_string();

    // Execute within transaction
    let resp = client
        .post(format!("{base}/tx/query"))
        .header("X-Session-Id", &session_id)
        .json(&json!({"query": "CREATE (n:TxItem {val: 42}) RETURN n.val"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Commit
    let resp = client
        .post(format!("{base}/tx/commit"))
        .header("X-Session-Id", &session_id)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Verify data is in "txdb"
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n:TxItem) RETURN n.val", "database": "txdb"}))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["rows"][0][0], 42);

    // Verify data is NOT in default
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n:TxItem) RETURN n.val"}))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    assert!(body["rows"].as_array().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Database creation options (v0.2.0)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_database_with_options() {
    let base = spawn_server().await;
    let client = Client::new();

    // Create with explicit type and options
    let resp = client
        .post(format!("{base}/db"))
        .json(&json!({
            "name": "custom-db",
            "database_type": "Lpg",
            "storage_mode": "InMemory",
            "options": {
                "memory_limit_bytes": 128 * 1024 * 1024,
                "backward_edges": false,
                "wal_enabled": false
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "custom-db");
    assert_eq!(body["database_type"], "lpg");

    // Verify info endpoint reflects settings
    let resp = client
        .get(format!("{base}/db/custom-db"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["database_type"], "lpg");
    assert_eq!(body["storage_mode"], "in-memory");
    assert_eq!(body["backward_edges"], false);
}

#[tokio::test]
async fn create_rdf_database() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/db"))
        .json(&json!({
            "name": "rdf-store",
            "database_type": "Rdf"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["database_type"], "rdf");

    // SPARQL should work on an RDF database
    let resp = client
        .post(format!("{base}/sparql"))
        .json(&json!({
            "query": "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1",
            "database": "rdf-store"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // List should show type badge
    let resp = client.get(format!("{base}/db")).send().await.unwrap();
    let body: Value = resp.json().await.unwrap();
    let dbs = body["databases"].as_array().unwrap();
    let rdf_db = dbs.iter().find(|d| d["name"] == "rdf-store").unwrap();
    assert_eq!(rdf_db["database_type"], "rdf");
}

#[tokio::test]
async fn persistent_rejected_without_data_dir() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/db"))
        .json(&json!({
            "name": "persist-fail",
            "storage_mode": "Persistent"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let body: Value = resp.json().await.unwrap();
    assert!(body["detail"].as_str().unwrap().contains("data-dir"));
}

#[tokio::test]
async fn system_resources_endpoint() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/system/resources"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert!(body["total_memory_bytes"].as_u64().unwrap() > 0);
    assert!(body["available_memory_bytes"].as_u64().unwrap() > 0);
    assert_eq!(body["persistent_available"], false); // in-memory server

    let types = body["available_types"].as_array().unwrap();
    assert!(types.iter().any(|t| t == "Lpg"));

    // Defaults should be present
    let defaults = &body["defaults"];
    assert!(defaults["memory_limit_bytes"].as_u64().unwrap() > 0);
    assert_eq!(defaults["backward_edges"], true);
    assert!(defaults["threads"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn system_resources_updates_after_create_delete() {
    let base = spawn_server().await;
    let client = Client::new();

    // Get initial allocated memory
    let resp = client
        .get(format!("{base}/system/resources"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let initial_allocated = body["allocated_memory_bytes"].as_u64().unwrap();

    // Create a new database
    client
        .post(format!("{base}/db"))
        .json(&json!({"name": "alloc-test"}))
        .send()
        .await
        .unwrap();

    // Allocated memory should increase
    let resp = client
        .get(format!("{base}/system/resources"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let after_create = body["allocated_memory_bytes"].as_u64().unwrap();
    assert!(after_create > initial_allocated);

    // Delete the database
    client
        .delete(format!("{base}/db/alloc-test"))
        .send()
        .await
        .unwrap();

    // Allocated memory should go back down
    let resp = client
        .get(format!("{base}/system/resources"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let after_delete = body["allocated_memory_bytes"].as_u64().unwrap();
    assert_eq!(after_delete, initial_allocated);
}

#[tokio::test]
async fn database_info_includes_new_fields() {
    let base = spawn_server().await;
    let client = Client::new();

    // Default database should have all new metadata fields
    let resp = client
        .get(format!("{base}/db/default"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["database_type"], "lpg");
    assert_eq!(body["storage_mode"], "in-memory");
    assert_eq!(body["backward_edges"], true);
    assert!(body["threads"].is_u64());
}

#[tokio::test]
async fn create_with_wal_durability_options() {
    let base = spawn_server().await;
    let client = Client::new();

    // Create with WAL durability option - creation should succeed
    let resp = client
        .post(format!("{base}/db"))
        .json(&json!({
            "name": "wal-test",
            "options": {
                "wal_durability": "sync"
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Verify the database was created and is queryable
    let resp = client
        .get(format!("{base}/db/wal-test"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "wal-test");
}

#[tokio::test]
async fn create_with_invalid_durability_returns_400() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/db"))
        .json(&json!({
            "name": "bad-wal",
            "options": {
                "wal_durability": "invalid-mode"
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn openapi_includes_system_resources_path() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/api/openapi.json"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();

    let paths = body["paths"].as_object().unwrap();
    assert!(paths.contains_key("/system/resources"));
}

// ---------------------------------------------------------------------------
// Compression
// ---------------------------------------------------------------------------

#[tokio::test]
async fn gzip_compression_when_requested() {
    let base = spawn_server().await;
    let client = Client::builder().gzip(true).build().unwrap();

    let resp = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    // reqwest transparently decompresses; just verify the response is valid
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn metrics_endpoint_returns_prometheus_format() {
    let base = spawn_server().await;
    let client = Client::new();

    // Run a query so counters have data
    client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n) RETURN count(n)"}))
        .send()
        .await
        .unwrap();

    let resp = client.get(format!("{base}/metrics")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.contains("text/plain"));

    let body = resp.text().await.unwrap();
    assert!(body.contains("grafeo_databases_total"));
    assert!(body.contains("grafeo_uptime_seconds"));
    assert!(body.contains("grafeo_active_sessions_total"));
    assert!(body.contains("grafeo_queries_total{language=\"gql\"}"));
}

// ---------------------------------------------------------------------------
// Query Timeout
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_with_timeout_ms_succeeds() {
    let base = spawn_server().await;
    let client = Client::new();

    // Large timeout — should succeed
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n) RETURN count(n)", "timeout_ms": 60000}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn query_with_timeout_zero_disables() {
    let base = spawn_server().await;
    let client = Client::new();

    // timeout_ms: 0 means disabled for this query
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n) RETURN count(n)", "timeout_ms": 0}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

// ---------------------------------------------------------------------------
// Authentication (feature-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "auth")]
async fn spawn_server_with_auth(token: &str) -> String {
    let state = grafeo_server::AppState::new_in_memory_with_auth(300, token.to_string());
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

#[cfg(feature = "auth")]
#[tokio::test]
async fn auth_required_when_configured() {
    let base = spawn_server_with_auth("secret-token").await;
    let client = Client::new();

    // No token -> 401
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n) RETURN count(n)"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // Wrong token -> 401
    let resp = client
        .post(format!("{base}/query"))
        .header("Authorization", "Bearer wrong")
        .json(&json!({"query": "MATCH (n) RETURN count(n)"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // Correct token -> 200
    let resp = client
        .post(format!("{base}/query"))
        .header("Authorization", "Bearer secret-token")
        .json(&json!({"query": "MATCH (n) RETURN count(n)"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn health_exempt_from_auth() {
    let base = spawn_server_with_auth("secret-token").await;
    let client = Client::new();

    let resp = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn metrics_exempt_from_auth() {
    let base = spawn_server_with_auth("secret-token").await;
    let client = Client::new();

    let resp = client.get(format!("{base}/metrics")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn no_auth_when_not_configured() {
    let base = spawn_server().await;
    let client = Client::new();

    // Standard spawn_server has no auth — should work without token
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n) RETURN count(n)"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn api_key_auth_works() {
    let base = spawn_server_with_auth("secret-token").await;
    let client = Client::new();

    // X-API-Key header accepted
    let resp = client
        .post(format!("{base}/query"))
        .header("X-API-Key", "secret-token")
        .json(&json!({"query": "MATCH (n) RETURN count(n)"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Wrong API key -> 401
    let resp = client
        .post(format!("{base}/query"))
        .header("X-API-Key", "wrong-key")
        .json(&json!({"query": "MATCH (n) RETURN count(n)"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[cfg(feature = "auth")]
async fn spawn_server_with_basic_auth(user: &str, password: &str) -> String {
    let state = grafeo_server::AppState::new_in_memory_with_basic_auth(
        300,
        user.to_string(),
        password.to_string(),
    );
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

#[cfg(feature = "auth")]
#[tokio::test]
async fn basic_auth_works() {
    let base = spawn_server_with_basic_auth("admin", "s3cret").await;
    let client = Client::new();

    // No auth -> 401
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n) RETURN count(n)"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // Correct Basic auth -> 200
    use base64::Engine as _;
    let creds = base64::engine::general_purpose::STANDARD.encode("admin:s3cret");
    let resp = client
        .post(format!("{base}/query"))
        .header("Authorization", format!("Basic {creds}"))
        .json(&json!({"query": "MATCH (n) RETURN count(n)"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn basic_auth_wrong_password_returns_401() {
    let base = spawn_server_with_basic_auth("admin", "s3cret").await;
    let client = Client::new();

    use base64::Engine as _;
    let creds = base64::engine::general_purpose::STANDARD.encode("admin:wrong");
    let resp = client
        .post(format!("{base}/query"))
        .header("Authorization", format!("Basic {creds}"))
        .json(&json!({"query": "MATCH (n) RETURN count(n)"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn basic_auth_exempt_paths() {
    let base = spawn_server_with_basic_auth("admin", "s3cret").await;
    let client = Client::new();

    // Health is always exempt
    let resp = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // Metrics is always exempt
    let resp = client.get(format!("{base}/metrics")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
}

// ---------------------------------------------------------------------------
// Batch queries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn batch_query_empty() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/batch"))
        .json(&json!({"queries": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["results"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn batch_query_multiple_writes() {
    let base = spawn_server().await;
    let client = Client::new();

    // Batch: create two nodes, then count
    let resp = client
        .post(format!("{base}/batch"))
        .json(&json!({
            "queries": [
                {"query": "CREATE (n:BatchTest {name: 'Alice'})"},
                {"query": "CREATE (n:BatchTest {name: 'Bob'})"},
                {"query": "MATCH (n:BatchTest) RETURN count(n) AS cnt"}
            ],
            "language": "cypher"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 3);

    // The third query should see both nodes (same transaction)
    assert_eq!(results[2]["rows"][0][0], 2);
}

#[tokio::test]
async fn batch_query_rolls_back_on_error() {
    let base = spawn_server().await;
    let client = Client::new();

    // First: create a node via normal query
    client
        .post(format!("{base}/query"))
        .json(&json!({"query": "CREATE (n:Survivor {id: 1})"}))
        .send()
        .await
        .unwrap();

    // Batch: create a node, then fail with bad syntax
    let resp = client
        .post(format!("{base}/batch"))
        .json(&json!({
            "queries": [
                {"query": "CREATE (n:Ghost {id: 99})"},
                {"query": "THIS IS NOT VALID SYNTAX"}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert!(body["detail"].as_str().unwrap().contains("index 1"));

    // Ghost node should not exist (rolled back)
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n:Ghost) RETURN count(n) AS cnt"}))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["rows"][0][0], 0);
}

#[tokio::test]
async fn batch_query_on_specific_database() {
    let base = spawn_server().await;
    let client = Client::new();

    // Create a database
    client
        .post(format!("{base}/db"))
        .json(&json!({"name": "batch_test_db"}))
        .send()
        .await
        .unwrap();

    // Batch on that database
    let resp = client
        .post(format!("{base}/batch"))
        .json(&json!({
            "queries": [
                {"query": "CREATE (n:X {val: 42})"},
                {"query": "MATCH (n:X) RETURN n.val AS v"}
            ],
            "database": "batch_test_db"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["results"][1]["rows"][0][0], 42);
}

// ---------------------------------------------------------------------------
// Rate limiting
// ---------------------------------------------------------------------------

async fn spawn_server_with_rate_limit(max_requests: u64) -> String {
    let state = grafeo_server::AppState::new_in_memory_with_rate_limit(
        300,
        max_requests,
        std::time::Duration::from_secs(60),
    );
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

#[tokio::test]
async fn rate_limit_returns_429_when_exceeded() {
    let base = spawn_server_with_rate_limit(3).await;
    let client = Client::new();

    // First 3 requests should succeed
    for _ in 0..3 {
        let resp = client.get(format!("{base}/health")).send().await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    // 4th request should be rate-limited
    let resp = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(resp.status(), 429);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "too_many_requests");
}

#[tokio::test]
async fn rate_limit_disabled_when_zero() {
    // Default spawn_server has rate_limit = 0 (disabled)
    let base = spawn_server().await;
    let client = Client::new();

    // Many requests should all succeed
    for _ in 0..20 {
        let resp = client.get(format!("{base}/health")).send().await.unwrap();
        assert_eq!(resp.status(), 200);
    }
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

#[tokio::test]
async fn websocket_query() {
    let base = spawn_server().await;
    let ws_url = base.replace("http://", "ws://") + "/ws";

    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("WebSocket connect failed");

    // Send a query
    let msg = json!({
        "type": "query",
        "id": "q1",
        "query": "MATCH (n) RETURN count(n)"
    });
    ws.send(tungstenite::Message::Text(msg.to_string().into()))
        .await
        .unwrap();

    // Receive result
    let reply = ws.next().await.unwrap().unwrap();
    let body: Value = serde_json::from_str(reply.to_text().unwrap()).unwrap();
    assert_eq!(body["type"], "result");
    assert_eq!(body["id"], "q1");
    assert!(body["columns"].is_array());
    assert!(body["rows"].is_array());
}

#[tokio::test]
async fn websocket_ping_pong() {
    let base = spawn_server().await;
    let ws_url = base.replace("http://", "ws://") + "/ws";

    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();

    ws.send(tungstenite::Message::Text(
        json!({"type": "ping"}).to_string().into(),
    ))
    .await
    .unwrap();

    let reply = ws.next().await.unwrap().unwrap();
    let body: Value = serde_json::from_str(reply.to_text().unwrap()).unwrap();
    assert_eq!(body["type"], "pong");
}

#[tokio::test]
async fn websocket_bad_message() {
    let base = spawn_server().await;
    let ws_url = base.replace("http://", "ws://") + "/ws";

    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();

    ws.send(tungstenite::Message::Text("not json".into()))
        .await
        .unwrap();

    let reply = ws.next().await.unwrap().unwrap();
    let body: Value = serde_json::from_str(reply.to_text().unwrap()).unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"], "bad_request");
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn websocket_auth_required() {
    let base = spawn_server_with_auth("secret-token").await;
    let ws_url = base.replace("http://", "ws://") + "/ws";

    // Without auth header → upgrade should fail with 401
    let result = tokio_tungstenite::connect_async(&ws_url).await;
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// CALL Procedures (v0.2.4)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn call_procedures_list_via_gql() {
    let base = spawn_server().await;
    let client = Client::new();

    // List all available procedures
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "CALL grafeo.procedures() YIELD name, description"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    let columns = body["columns"].as_array().unwrap();
    assert!(columns.iter().any(|c| c == "name"));
    assert!(columns.iter().any(|c| c == "description"));

    let rows = body["rows"].as_array().unwrap();
    assert!(!rows.is_empty(), "should list at least one procedure");

    // Verify known algorithms are present
    let names: Vec<&str> = rows.iter().filter_map(|r| r[0].as_str()).collect();
    assert!(
        names.contains(&"grafeo.pagerank"),
        "pagerank should be registered"
    );
    assert!(names.contains(&"grafeo.bfs"), "bfs should be registered");
    assert!(
        names.contains(&"grafeo.connected_components"),
        "wcc should be registered"
    );
}

#[tokio::test]
async fn call_pagerank_via_gql() {
    let base = spawn_server().await;
    let client = Client::new();

    // Seed a small graph via Cypher
    client
        .post(format!("{base}/cypher"))
        .json(&json!({"query": "CREATE (:Page {name: 'A'})-[:LINKS_TO]->(:Page {name: 'B'})-[:LINKS_TO]->(:Page {name: 'C'})"}))
        .send()
        .await
        .unwrap();

    // Run PageRank via CALL (GQL)
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "CALL grafeo.pagerank({damping: 0.85, iterations: 20}) YIELD node_id, score"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    let columns = body["columns"].as_array().unwrap();
    assert!(columns.iter().any(|c| c == "node_id"));
    assert!(columns.iter().any(|c| c == "score"));

    let rows = body["rows"].as_array().unwrap();
    assert!(
        !rows.is_empty(),
        "pagerank should return results for seeded graph"
    );
}

#[tokio::test]
async fn call_connected_components_via_cypher() {
    let base = spawn_server().await;
    let client = Client::new();

    // Seed graph with two components
    client
        .post(format!("{base}/cypher"))
        .json(&json!({"query": "CREATE (:Node {name: 'A'})-[:EDGE]->(:Node {name: 'B'})"}))
        .send()
        .await
        .unwrap();
    client
        .post(format!("{base}/cypher"))
        .json(&json!({"query": "CREATE (:Node {name: 'C'})"}))
        .send()
        .await
        .unwrap();

    // Run WCC via CALL (Cypher)
    let resp = client
        .post(format!("{base}/cypher"))
        .json(&json!({"query": "CALL grafeo.connected_components() YIELD node_id, component_id"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    let columns = body["columns"].as_array().unwrap();
    assert!(columns.iter().any(|c| c == "node_id"));
    assert!(columns.iter().any(|c| c == "component_id"));

    let rows = body["rows"].as_array().unwrap();
    assert!(rows.len() >= 3, "should return a row per node");
}

#[tokio::test]
async fn call_unknown_procedure_returns_400() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "CALL grafeo.nonexistent_algorithm() YIELD x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// ---------------------------------------------------------------------------
// SQL/PGQ endpoint (v0.2.4)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sql_endpoint_call_procedures() {
    let base = spawn_server().await;
    let client = Client::new();

    // List procedures via SQL/PGQ endpoint
    let resp = client
        .post(format!("{base}/sql"))
        .json(&json!({"query": "CALL grafeo.procedures() YIELD name, description"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    let rows = body["rows"].as_array().unwrap();
    assert!(!rows.is_empty());
}

#[tokio::test]
async fn sql_pgq_via_query_language_field() {
    let base = spawn_server().await;
    let client = Client::new();

    // Use the /query endpoint with language: "sql-pgq"
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({
            "query": "CALL grafeo.procedures() YIELD name, description",
            "language": "sql-pgq"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert!(!body["rows"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn openapi_includes_sql_path() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/api/openapi.json"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();

    let paths = body["paths"].as_object().unwrap();
    assert!(paths.contains_key("/sql"));
}

#[tokio::test]
async fn metrics_tracks_sql_pgq() {
    let base = spawn_server().await;
    let client = Client::new();

    // Run a SQL/PGQ query
    client
        .post(format!("{base}/sql"))
        .json(&json!({"query": "CALL grafeo.procedures() YIELD name"}))
        .send()
        .await
        .unwrap();

    // Check metrics include sql-pgq counter
    let resp = client.get(format!("{base}/metrics")).send().await.unwrap();
    let body = resp.text().await.unwrap();
    assert!(body.contains("language=\"sql-pgq\""));
}

// ---------------------------------------------------------------------------
// GQL Wire Protocol (v0.3.0)
// ---------------------------------------------------------------------------

/// Boots an in-memory Grafeo server with both HTTP and GWP (gRPC) ports.
/// Returns `(http_base_url, gwp_endpoint)`.
#[cfg(feature = "gwp")]
async fn spawn_server_with_gwp() -> (String, String) {
    use grafeo_service::types::EnabledFeatures;

    let service = grafeo_service::ServiceState::new_in_memory(300);
    let features = EnabledFeatures {
        languages: vec![],
        engine: vec![],
        server: vec!["gwp".to_string()],
    };
    let state = grafeo_server::AppState::new(service, vec![], features);
    let app = grafeo_server::router(state.clone());

    // HTTP
    let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr: SocketAddr = http_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            http_listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    // GWP (gRPC)
    let gwp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gwp_addr: SocketAddr = gwp_listener.local_addr().unwrap();
    // Drop the listener so tonic can bind the same port
    drop(gwp_listener);
    let backend = grafeo_gwp::GrafeoBackend::new(state.service().clone());
    tokio::spawn(async move {
        gwp::server::GqlServer::start(backend, gwp_addr)
            .await
            .unwrap();
    });
    // Give the gRPC server a moment to bind
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    (format!("http://{http_addr}"), format!("http://{gwp_addr}"))
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_session_create_and_close() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let conn = gwp::client::GqlConnection::connect(&gwp_endpoint)
        .await
        .expect("GWP connect failed");

    let session = conn
        .create_session()
        .await
        .expect("GWP create_session failed");

    let session_id = session.session_id().to_owned();
    assert!(!session_id.is_empty(), "session ID should not be empty");

    session.close().await.expect("GWP close_session failed");
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_execute_query() {
    let (http, gwp_endpoint) = spawn_server_with_gwp().await;
    let http_client = Client::new();

    // Seed data via HTTP (Cypher CREATE)
    let resp = http_client
        .post(format!("{http}/cypher"))
        .json(&json!({"query": "CREATE (:GwpTest {name: 'Alice'})-[:KNOWS]->(:GwpTest {name: 'Bob'})"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Query via GWP
    let conn = gwp::client::GqlConnection::connect(&gwp_endpoint)
        .await
        .unwrap();
    let mut session = conn.create_session().await.unwrap();

    let mut cursor = session
        .execute(
            "MATCH (n:GwpTest) RETURN n.name ORDER BY n.name",
            std::collections::HashMap::new(),
        )
        .await
        .expect("GWP execute failed");

    let columns = cursor.column_names().await.unwrap();
    assert!(!columns.is_empty(), "should have column names");

    let rows = cursor.collect_rows().await.unwrap();
    assert_eq!(rows.len(), 2, "should find 2 GwpTest nodes");

    session.close().await.unwrap();
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_transaction_commit() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let conn = gwp::client::GqlConnection::connect(&gwp_endpoint)
        .await
        .unwrap();
    let mut session = conn.create_session().await.unwrap();

    // Begin transaction, create a node, commit
    let mut tx = session.begin_transaction().await.unwrap();
    let mut cursor = tx
        .execute(
            "CREATE (:TxTest {val: 42})",
            std::collections::HashMap::new(),
        )
        .await
        .unwrap();
    let _ = cursor.collect_rows().await.unwrap();
    tx.commit().await.unwrap();

    // Verify committed data is visible
    let mut cursor = session
        .execute(
            "MATCH (n:TxTest) RETURN n.val",
            std::collections::HashMap::new(),
        )
        .await
        .unwrap();
    let rows = cursor.collect_rows().await.unwrap();
    assert_eq!(rows.len(), 1, "committed node should be visible");

    session.close().await.unwrap();
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_health_reports_gwp_feature() {
    let (http, _gwp) = spawn_server_with_gwp().await;
    let client = Client::new();

    let resp = client.get(format!("{http}/health")).send().await.unwrap();
    let body: Value = resp.json().await.unwrap();

    let server_features = body["features"]["server"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>();
    assert!(
        server_features.contains(&"gwp"),
        "health should report gwp feature; got: {server_features:?}"
    );
}

// ---------------------------------------------------------------------------
// GWP Database Lifecycle (v0.4.1)
// ---------------------------------------------------------------------------

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_list_databases_returns_default() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let conn = gwp::client::GqlConnection::connect(&gwp_endpoint)
        .await
        .unwrap();
    let mut catalog_client = conn.create_catalog_client();

    let databases = catalog_client.list_graphs("default").await.unwrap();
    assert_eq!(databases.len(), 1);
    assert_eq!(databases[0].name, "default");
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_create_and_delete_database() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let conn = gwp::client::GqlConnection::connect(&gwp_endpoint)
        .await
        .unwrap();
    let mut catalog_client = conn.create_catalog_client();

    // Create a database
    let info = catalog_client
        .create_graph(gwp::server::CreateGraphConfig {
            schema: "default".to_string(),
            name: "gwp-test-db".to_string(),
            if_not_exists: false,
            or_replace: false,
            type_spec: None,
            copy_of: None,
            storage_mode: "inmemory".to_string(),
            memory_limit_bytes: None,
            backward_edges: None,
            threads: None,
            wal_enabled: None,
            wal_durability: None,
        })
        .await
        .unwrap();
    assert_eq!(info.name, "gwp-test-db");
    assert_eq!(info.node_count, 0);

    // List should show 2 databases
    let databases = catalog_client.list_graphs("default").await.unwrap();
    assert_eq!(databases.len(), 2);

    // Delete
    let deleted = catalog_client
        .drop_graph("default", "gwp-test-db", false)
        .await
        .unwrap();
    assert!(deleted);

    // List should show 1 database
    let databases = catalog_client.list_graphs("default").await.unwrap();
    assert_eq!(databases.len(), 1);
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_get_database_info() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let conn = gwp::client::GqlConnection::connect(&gwp_endpoint)
        .await
        .unwrap();
    let mut catalog_client = conn.create_catalog_client();

    let info = catalog_client
        .get_graph_info("default", "default")
        .await
        .unwrap();
    assert_eq!(info.name, "default");
    assert_eq!(info.graph_type, "lpg");
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_create_database_then_query() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let conn = gwp::client::GqlConnection::connect(&gwp_endpoint)
        .await
        .unwrap();

    // Create database via GWP
    let mut catalog_client = conn.create_catalog_client();
    catalog_client
        .create_graph(gwp::server::CreateGraphConfig {
            schema: "default".to_string(),
            name: "query-db".to_string(),
            if_not_exists: false,
            or_replace: false,
            type_spec: None,
            copy_of: None,
            storage_mode: "inmemory".to_string(),
            memory_limit_bytes: None,
            backward_edges: None,
            threads: None,
            wal_enabled: None,
            wal_durability: None,
        })
        .await
        .unwrap();

    // Create session, configure to the new database, execute query
    let mut session = conn.create_session().await.unwrap();
    session.set_graph("query-db").await.unwrap();

    let mut cursor = session
        .execute(
            "CREATE (:Widget {name: 'Gear'}) RETURN 'ok'",
            std::collections::HashMap::new(),
        )
        .await
        .unwrap();
    let rows = cursor.collect_rows().await.unwrap();
    assert_eq!(rows.len(), 1);

    // Verify node count via get_info
    let info = catalog_client
        .get_graph_info("default", "query-db")
        .await
        .unwrap();
    assert_eq!(info.node_count, 1);

    session.close().await.unwrap();
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_delete_then_recreate_database() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let conn = gwp::client::GqlConnection::connect(&gwp_endpoint)
        .await
        .unwrap();
    let mut catalog_client = conn.create_catalog_client();

    let config = gwp::server::CreateGraphConfig {
        schema: "default".to_string(),
        name: "ephemeral".to_string(),
        if_not_exists: false,
        or_replace: false,
        type_spec: None,
        copy_of: None,
        storage_mode: "inmemory".to_string(),
        memory_limit_bytes: None,
        backward_edges: None,
        threads: None,
        wal_enabled: None,
        wal_durability: None,
    };

    // Create, delete, recreate — exercises the close barrier path
    catalog_client.create_graph(config.clone()).await.unwrap();
    catalog_client
        .drop_graph("default", "ephemeral", false)
        .await
        .unwrap();
    catalog_client.create_graph(config).await.unwrap();

    // Should be queryable
    let mut session = conn.create_session().await.unwrap();
    session.set_graph("ephemeral").await.unwrap();

    let mut cursor = session
        .execute(
            "MATCH (n) RETURN count(n)",
            std::collections::HashMap::new(),
        )
        .await
        .unwrap();
    let rows = cursor.collect_rows().await.unwrap();
    assert_eq!(rows.len(), 1, "should return a count row");

    session.close().await.unwrap();
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_delete_nonexistent_database_fails() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let conn = gwp::client::GqlConnection::connect(&gwp_endpoint)
        .await
        .unwrap();
    let mut catalog_client = conn.create_catalog_client();

    let result = catalog_client
        .drop_graph("default", "nonexistent", false)
        .await;
    assert!(result.is_err(), "deleting nonexistent database should fail");
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_create_duplicate_database_fails() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let conn = gwp::client::GqlConnection::connect(&gwp_endpoint)
        .await
        .unwrap();
    let mut catalog_client = conn.create_catalog_client();

    let config = gwp::server::CreateGraphConfig {
        schema: "default".to_string(),
        name: "dup".to_string(),
        if_not_exists: false,
        or_replace: false,
        type_spec: None,
        copy_of: None,
        storage_mode: "inmemory".to_string(),
        memory_limit_bytes: None,
        backward_edges: None,
        threads: None,
        wal_enabled: None,
        wal_durability: None,
    };

    catalog_client.create_graph(config.clone()).await.unwrap();
    let result = catalog_client.create_graph(config).await;
    assert!(result.is_err(), "creating duplicate database should fail");
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_configure_deleted_database_fails() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let conn = gwp::client::GqlConnection::connect(&gwp_endpoint)
        .await
        .unwrap();
    let mut catalog_client = conn.create_catalog_client();

    // Create then delete a database
    catalog_client
        .create_graph(gwp::server::CreateGraphConfig {
            schema: "default".to_string(),
            name: "doomed".to_string(),
            if_not_exists: false,
            or_replace: false,
            type_spec: None,
            copy_of: None,
            storage_mode: "inmemory".to_string(),
            memory_limit_bytes: None,
            backward_edges: None,
            threads: None,
            wal_enabled: None,
            wal_durability: None,
        })
        .await
        .unwrap();
    catalog_client
        .drop_graph("default", "doomed", false)
        .await
        .unwrap();

    // Configuring a session to the deleted database should fail
    let mut session = conn.create_session().await.unwrap();
    let result = session.set_graph("doomed").await;
    assert!(
        result.is_err(),
        "configuring session to deleted database should fail"
    );

    session.close().await.unwrap();
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_list_schemas() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let conn = gwp::client::GqlConnection::connect(&gwp_endpoint)
        .await
        .unwrap();
    let mut catalog_client = conn.create_catalog_client();

    let schemas = catalog_client.list_schemas().await.unwrap();
    assert_eq!(schemas.len(), 1);
    assert_eq!(schemas[0].name, "default");
    assert!(schemas[0].graph_count > 0);
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_schema_operations() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let conn = gwp::client::GqlConnection::connect(&gwp_endpoint)
        .await
        .unwrap();
    let mut catalog_client = conn.create_catalog_client();

    // Creating a new schema should succeed.
    catalog_client
        .create_schema("analytics", false)
        .await
        .unwrap();

    // Creating it again with if_not_exists should succeed (no-op).
    catalog_client
        .create_schema("analytics", true)
        .await
        .unwrap();

    // Dropping it should succeed.
    let dropped = catalog_client
        .drop_schema("analytics", false)
        .await
        .unwrap();
    assert!(dropped);

    // Dropping nonexistent schema without if_exists should fail.
    let result = catalog_client.drop_schema("nonexistent", false).await;
    assert!(result.is_err());

    // Dropping nonexistent schema with if_exists should succeed (no-op).
    let dropped = catalog_client
        .drop_schema("nonexistent", true)
        .await
        .unwrap();
    assert!(!dropped);
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_graph_type_stubs() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let conn = gwp::client::GqlConnection::connect(&gwp_endpoint)
        .await
        .unwrap();
    let mut catalog_client = conn.create_catalog_client();

    // list_graph_types returns empty
    let types = catalog_client.list_graph_types("default").await.unwrap();
    assert!(types.is_empty());

    // create_graph_type returns error
    let result = catalog_client
        .create_graph_type("default", "MyType", false, false)
        .await;
    assert!(result.is_err());

    // drop_graph_type returns error
    let result = catalog_client
        .drop_graph_type("default", "MyType", false)
        .await;
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Admin endpoints (v0.4.3)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_stats_returns_valid_response() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/admin/default/stats"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "default");
    assert_eq!(body["node_count"], 0);
    assert_eq!(body["edge_count"], 0);
    assert!(body["memory_bytes"].is_u64());
}

#[tokio::test]
async fn admin_stats_not_found() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/admin/nonexistent/stats"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn admin_wal_status_in_memory() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/admin/default/wal"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["enabled"], false);
    assert!(body["current_epoch"].is_u64());
}

#[tokio::test]
async fn admin_wal_checkpoint_succeeds() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/admin/default/wal/checkpoint"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["success"], true);
}

#[tokio::test]
async fn admin_write_snapshot_in_memory_errors() {
    let base = spawn_server().await;
    let client = Client::new();

    // In-memory server: snapshot will fail (no file manager), but endpoint exists.
    let resp = client
        .post(format!("{base}/admin/default/snapshot"))
        .send()
        .await
        .unwrap();
    // Should not be 404 (endpoint exists), expect 400 or 500 depending on features.
    assert_ne!(resp.status(), 404);
}

#[tokio::test]
async fn admin_write_snapshot_not_found() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/admin/nonexistent/snapshot"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn admin_validate_clean_database() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/admin/default/validate"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["valid"], true);
    assert!(body["errors"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn admin_create_and_drop_property_index() {
    let base = spawn_server().await;
    let client = Client::new();

    // Create index
    let resp = client
        .post(format!("{base}/admin/default/index"))
        .json(&json!({"type": "property", "property": "name"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["created"], true);

    // Drop index
    let resp = client
        .delete(format!("{base}/admin/default/index"))
        .json(&json!({"type": "property", "property": "name"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["dropped"], true);

    // Drop again — should return false
    let resp = client
        .delete(format!("{base}/admin/default/index"))
        .json(&json!({"type": "property", "property": "name"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["dropped"], false);
}

#[tokio::test]
async fn admin_stats_after_data_insertion() {
    let base = spawn_server().await;
    let client = Client::new();

    // Insert data (two separate known-good INSERT statements)
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "INSERT (:Person {name: 'Alice'})"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "INSERT (:Person {name: 'Bob'})"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Stats should reflect the data
    let resp = client
        .get(format!("{base}/admin/default/stats"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["node_count"], 2);
    assert!(body["label_count"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn openapi_includes_admin_and_search_paths() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/api/openapi.json"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();

    let paths = body["paths"].as_object().unwrap();
    assert!(paths.contains_key("/admin/{db}/stats"));
    assert!(paths.contains_key("/admin/{db}/wal"));
    assert!(paths.contains_key("/admin/{db}/wal/checkpoint"));
    assert!(paths.contains_key("/admin/{db}/validate"));
    assert!(paths.contains_key("/admin/{db}/index"));
    assert!(paths.contains_key("/search/vector"));
    assert!(paths.contains_key("/search/text"));
    assert!(paths.contains_key("/search/hybrid"));
    assert!(paths.contains_key("/admin/{db}/memory"));
    assert!(paths.contains_key("/db/{name}/graphs"));
    assert!(paths.contains_key("/db/{name}/graphs/{graph}"));
}

// ---------------------------------------------------------------------------
// Search endpoints (v0.4.3) — feature-dependent stubs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_vector_requires_database() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/search/vector"))
        .json(&json!({
            "database": "nonexistent",
            "label": "Document",
            "property": "embedding",
            "query_vector": [0.1, 0.2, 0.3],
            "k": 5
        }))
        .send()
        .await
        .unwrap();
    // Should return 400 (feature disabled) or 404 (db not found)
    assert!(resp.status() == 400 || resp.status() == 404);
}

#[tokio::test]
async fn search_text_requires_database() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/search/text"))
        .json(&json!({
            "database": "nonexistent",
            "label": "Document",
            "property": "content",
            "query": "hello world",
            "k": 5
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status() == 400 || resp.status() == 404);
}

// ---------------------------------------------------------------------------
// GWP Admin operations (v0.4.3)
// ---------------------------------------------------------------------------

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_admin_stats() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let channel = tonic::transport::Channel::from_shared(gwp_endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut admin_client = gwp::proto::admin_service_client::AdminServiceClient::new(channel);

    let resp = admin_client
        .get_graph_stats(gwp::proto::GetGraphStatsRequest {
            graph: "default".to_string(),
        })
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.node_count, 0);
    assert_eq!(resp.edge_count, 0);
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_admin_wal_status() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let channel = tonic::transport::Channel::from_shared(gwp_endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut admin_client = gwp::proto::admin_service_client::AdminServiceClient::new(channel);

    let resp = admin_client
        .wal_status(gwp::proto::WalStatusRequest {
            graph: "default".to_string(),
        })
        .await
        .unwrap()
        .into_inner();

    assert!(!resp.enabled);
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_admin_validate() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let channel = tonic::transport::Channel::from_shared(gwp_endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut admin_client = gwp::proto::admin_service_client::AdminServiceClient::new(channel);

    let resp = admin_client
        .validate(gwp::proto::ValidateRequest {
            graph: "default".to_string(),
        })
        .await
        .unwrap()
        .into_inner();

    assert!(resp.valid);
    assert!(resp.errors.is_empty());
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_admin_create_index() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let channel = tonic::transport::Channel::from_shared(gwp_endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut admin_client = gwp::proto::admin_service_client::AdminServiceClient::new(channel);

    // Create a property index
    admin_client
        .create_index(gwp::proto::CreateIndexRequest {
            graph: "default".to_string(),
            index: Some(gwp::proto::create_index_request::Index::PropertyIndex(
                gwp::proto::PropertyIndexDef {
                    property: "name".to_string(),
                },
            )),
        })
        .await
        .unwrap();

    // Drop the index
    let resp = admin_client
        .drop_index(gwp::proto::DropIndexRequest {
            graph: "default".to_string(),
            index: Some(gwp::proto::drop_index_request::Index::PropertyIndex(
                gwp::proto::PropertyIndexDef {
                    property: "name".to_string(),
                },
            )),
        })
        .await
        .unwrap()
        .into_inner();

    assert!(resp.existed);
}

// ===========================================================================
// Bolt v5 protocol integration tests
// ===========================================================================

#[cfg(feature = "bolt")]
async fn spawn_server_with_bolt() -> (String, SocketAddr) {
    use grafeo_service::types::EnabledFeatures;

    let service = grafeo_service::ServiceState::new_in_memory(300);
    let features = EnabledFeatures {
        languages: vec![],
        engine: vec![],
        server: vec!["bolt".to_string()],
    };
    let state = grafeo_server::AppState::new(service, vec![], features);
    let app = grafeo_server::router(state.clone());

    // HTTP
    let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr: SocketAddr = http_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            http_listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    // Bolt
    let bolt_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bolt_addr: SocketAddr = bolt_listener.local_addr().unwrap();
    drop(bolt_listener);
    let backend =
        grafeo_boltr::GrafeoBackend::new(state.service().clone()).with_advertise_addr(bolt_addr);
    tokio::spawn(async move {
        grafeo_boltr::serve(backend, bolt_addr, grafeo_boltr::BoltrOptions::default())
            .await
            .unwrap();
    });
    // Give the Bolt server time to bind
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    (format!("http://{http_addr}"), bolt_addr)
}

#[cfg(feature = "bolt")]
#[tokio::test]
async fn bolt_connect_and_authenticate() {
    let (_http, bolt_addr) = spawn_server_with_bolt().await;

    let session = boltr::client::BoltSession::connect(bolt_addr)
        .await
        .expect("Bolt connect failed");

    assert_eq!(session.version(), (5, 4));

    session.close().await.expect("Bolt close failed");
}

#[cfg(feature = "bolt")]
#[tokio::test]
async fn bolt_execute_query() {
    let (http, bolt_addr) = spawn_server_with_bolt().await;
    let http_client = Client::new();

    // Seed data via HTTP
    let resp = http_client
        .post(format!("{http}/cypher"))
        .json(&json!({"query": "CREATE (:BoltTest {name: 'Alice'})-[:KNOWS]->(:BoltTest {name: 'Bob'})"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Query via Bolt
    let mut session = boltr::client::BoltSession::connect(bolt_addr)
        .await
        .unwrap();

    let result = session
        .run("MATCH (n:BoltTest) RETURN n.name ORDER BY n.name")
        .await
        .expect("Bolt query failed");

    assert_eq!(result.columns, vec!["n.name"]);
    assert_eq!(result.records.len(), 2);

    session.close().await.unwrap();
}

#[cfg(feature = "bolt")]
#[tokio::test]
async fn bolt_transaction_commit() {
    let (_http, bolt_addr) = spawn_server_with_bolt().await;

    let mut session = boltr::client::BoltSession::connect(bolt_addr)
        .await
        .unwrap();

    // Begin transaction, create a node, commit
    session.begin().await.unwrap();
    let _ = session.run("CREATE (:BoltTxTest {val: 42})").await.unwrap();
    session.commit().await.unwrap();

    // Verify committed data is visible
    let result = session
        .run("MATCH (n:BoltTxTest) RETURN n.val")
        .await
        .unwrap();
    assert_eq!(result.records.len(), 1);

    session.close().await.unwrap();
}

#[cfg(feature = "bolt")]
#[tokio::test]
async fn bolt_transaction_rollback() {
    let (_http, bolt_addr) = spawn_server_with_bolt().await;

    let mut session = boltr::client::BoltSession::connect(bolt_addr)
        .await
        .unwrap();

    // Begin transaction, create a node, rollback
    session.begin().await.unwrap();
    let _ = session.run("CREATE (:BoltRbTest {val: 99})").await.unwrap();
    session.rollback().await.unwrap();

    // Verify rolled-back data is NOT visible
    let result = session
        .run("MATCH (n:BoltRbTest) RETURN n.val")
        .await
        .unwrap();
    assert_eq!(result.records.len(), 0);

    session.close().await.unwrap();
}

#[cfg(feature = "bolt")]
#[tokio::test]
async fn bolt_reset_from_failed() {
    let (_http, bolt_addr) = spawn_server_with_bolt().await;

    let mut session = boltr::client::BoltSession::connect(bolt_addr)
        .await
        .unwrap();

    // Cause a failure with bad syntax
    let err = session.run("THIS IS NOT VALID SYNTAX").await;
    assert!(err.is_err(), "bad syntax should fail");

    // RESET to recover
    session.reset().await.expect("RESET should succeed");

    // Should be able to run queries again
    let result = session.run("MATCH (n) RETURN n LIMIT 1").await.unwrap();
    assert!(result.records.len() <= 1);

    session.close().await.unwrap();
}

#[cfg(feature = "bolt")]
#[tokio::test]
async fn bolt_query_with_parameters() {
    let (_http, bolt_addr) = spawn_server_with_bolt().await;

    let mut session = boltr::client::BoltSession::connect(bolt_addr)
        .await
        .unwrap();

    // Create with parameters
    let mut params = std::collections::HashMap::new();
    params.insert(
        "name".to_string(),
        boltr::types::BoltValue::String("Charlie".to_string()),
    );

    let _ = session
        .run_with_params(
            "CREATE (:BoltParamTest {name: $name})",
            params,
            boltr::types::BoltDict::new(),
        )
        .await
        .unwrap();

    let result = session
        .run("MATCH (n:BoltParamTest) RETURN n.name")
        .await
        .unwrap();
    assert_eq!(result.records.len(), 1);

    session.close().await.unwrap();
}

#[cfg(feature = "bolt")]
#[tokio::test]
async fn bolt_health_reports_bolt_feature() {
    let (http, _bolt_addr) = spawn_server_with_bolt().await;
    let client = Client::new();

    let resp = client.get(format!("{http}/health")).send().await.unwrap();
    let body: Value = resp.json().await.unwrap();

    let server_features = body["features"]["server"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>();

    assert!(
        server_features.contains(&"bolt"),
        "health features should include 'bolt', got: {server_features:?}"
    );
}

#[cfg(feature = "bolt")]
#[tokio::test]
async fn bolt_multiple_queries_in_session() {
    let (_http, bolt_addr) = spawn_server_with_bolt().await;

    let mut session = boltr::client::BoltSession::connect(bolt_addr)
        .await
        .unwrap();

    // Run multiple queries on the same session
    let _ = session.run("CREATE (:BoltMulti {seq: 1})").await.unwrap();
    let _ = session.run("CREATE (:BoltMulti {seq: 2})").await.unwrap();
    let _ = session.run("CREATE (:BoltMulti {seq: 3})").await.unwrap();

    let result = session
        .run("MATCH (n:BoltMulti) RETURN n.seq ORDER BY n.seq")
        .await
        .unwrap();
    assert_eq!(result.records.len(), 3);

    session.close().await.unwrap();
}

#[cfg(feature = "bolt")]
#[tokio::test]
async fn bolt_server_info() {
    let (_http, bolt_addr) = spawn_server_with_bolt().await;

    // Use low-level connection to inspect HELLO response
    let mut conn = boltr::client::BoltConnection::connect(bolt_addr)
        .await
        .unwrap();

    let hello_meta = conn
        .hello(boltr::types::BoltDict::from([(
            "user_agent".to_string(),
            boltr::types::BoltValue::String("test-client".to_string()),
        )]))
        .await
        .unwrap();

    // Server should include server info
    let server = hello_meta
        .get("server")
        .and_then(|v| v.as_str())
        .expect("HELLO response should include 'server'");
    assert!(
        server.starts_with("GrafeoDB/"),
        "server string should start with 'GrafeoDB/', got: {server}"
    );

    conn.goodbye().await.ok();
}

#[cfg(feature = "bolt")]
#[tokio::test]
async fn bolt_database_switching() {
    let (http, bolt_addr) = spawn_server_with_bolt().await;
    let http_client = Client::new();

    // Create a second database via HTTP
    let resp = http_client
        .post(format!("{http}/db"))
        .json(&json!({"name": "bolt-switch-db"}))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "create db failed: {}",
        resp.status()
    );

    let mut session = boltr::client::BoltSession::connect(bolt_addr)
        .await
        .unwrap();

    // Create data on the new database using the `db` extra field
    let extra = boltr::types::BoltDict::from([(
        "db".to_string(),
        boltr::types::BoltValue::String("bolt-switch-db".to_string()),
    )]);
    let _ = session
        .run_with_params(
            "CREATE (:SwitchTest {name: 'A'})",
            std::collections::HashMap::new(),
            extra.clone(),
        )
        .await
        .unwrap();

    // Query the new database — should see the data
    let result = session
        .run_with_params(
            "MATCH (n:SwitchTest) RETURN n.name",
            std::collections::HashMap::new(),
            extra,
        )
        .await
        .unwrap();
    assert_eq!(result.records.len(), 1);

    // Query the default database explicitly — should NOT see the data
    let default_extra = boltr::types::BoltDict::from([(
        "db".to_string(),
        boltr::types::BoltValue::String("default".to_string()),
    )]);
    let result = session
        .run_with_params(
            "MATCH (n:SwitchTest) RETURN n.name",
            std::collections::HashMap::new(),
            default_extra,
        )
        .await
        .unwrap();
    assert_eq!(result.records.len(), 0);

    session.close().await.unwrap();
}

#[cfg(feature = "bolt")]
#[tokio::test]
async fn bolt_language_dispatch() {
    let (_http, bolt_addr) = spawn_server_with_bolt().await;

    let mut session = boltr::client::BoltSession::connect(bolt_addr)
        .await
        .unwrap();

    // Seed some data first (auto-detected as Cypher)
    let _ = session.run("CREATE (:LangTest {val: 1})").await.unwrap();

    // Run a Cypher query using the language extension
    let cypher_extra = boltr::types::BoltDict::from([(
        "language".to_string(),
        boltr::types::BoltValue::String("cypher".to_string()),
    )]);
    let result = session
        .run_with_params(
            "MATCH (n:LangTest) RETURN n.val AS value",
            std::collections::HashMap::new(),
            cypher_extra,
        )
        .await
        .unwrap();
    assert_eq!(result.columns, vec!["value"]);
    assert_eq!(result.records.len(), 1);

    // Run a SPARQL query using the language extension
    let sparql_extra = boltr::types::BoltDict::from([(
        "language".to_string(),
        boltr::types::BoltValue::String("sparql".to_string()),
    )]);
    let result = session
        .run_with_params(
            "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1",
            std::collections::HashMap::new(),
            sparql_extra,
        )
        .await
        .unwrap();
    // SPARQL should parse and execute (may return 0 rows on empty DB)
    assert!(result.columns.contains(&"s".to_string()));

    session.close().await.unwrap();
}

// ===========================================================================
// Memory usage endpoint (v0.4.7)
// ===========================================================================

#[tokio::test]
async fn admin_memory_usage_returns_breakdown() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/admin/default/memory"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert!(body["total_bytes"].is_u64());
    assert!(body["store"].is_object());
    assert!(body["store"]["total_bytes"].is_u64());
    assert!(body["indexes"].is_object());
    assert!(body["mvcc"].is_object());
    assert!(body["caches"].is_object());
    assert!(body["string_pool"].is_object());
    assert!(body["buffer_manager"].is_object());
}

#[tokio::test]
async fn admin_memory_usage_not_found() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/admin/nonexistent/memory"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// ===========================================================================
// Named graphs (v0.4.7)
// ===========================================================================

#[tokio::test]
async fn named_graphs_crud() {
    let base = spawn_server().await;
    let client = Client::new();

    // List graphs (initially empty)
    let resp = client
        .get(format!("{base}/db/default/graphs"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["graphs"].as_array().unwrap().is_empty());

    // Create a named graph
    let resp = client
        .post(format!("{base}/db/default/graphs"))
        .json(&json!({"name": "analytics"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["created"], true);

    // List again (should have one)
    let resp = client
        .get(format!("{base}/db/default/graphs"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["graphs"].as_array().unwrap().len(), 1);
    assert_eq!(body["graphs"][0], "analytics");

    // Create duplicate (should return false)
    let resp = client
        .post(format!("{base}/db/default/graphs"))
        .json(&json!({"name": "analytics"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["created"], false);

    // Drop the graph
    let resp = client
        .delete(format!("{base}/db/default/graphs/analytics"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["dropped"], true);

    // Drop again (should return false)
    let resp = client
        .delete(format!("{base}/db/default/graphs/analytics"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["dropped"], false);
}

#[tokio::test]
async fn named_graphs_database_not_found() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/db/nonexistent/graphs"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// ---------------------------------------------------------------------------
// Schema namespace endpoints
// ---------------------------------------------------------------------------

#[tokio::test]
async fn schema_namespace_crud() {
    let base = spawn_server().await;
    let client = Client::new();

    // List schemas on a fresh database (may be empty).
    let resp = client
        .get(format!("{base}/db/default/schemas"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["schemas"].is_array());

    // Create a schema.
    let resp = client
        .post(format!("{base}/db/default/schemas"))
        .json(&json!({ "name": "analytics" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["created"], true);

    // List schemas should include the new schema.
    let resp = client
        .get(format!("{base}/db/default/schemas"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let schemas = body["schemas"].as_array().unwrap();
    assert!(schemas.iter().any(|s| s.as_str() == Some("analytics")));

    // Drop the schema.
    let resp = client
        .delete(format!("{base}/db/default/schemas/analytics"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["dropped"], true);
}

#[tokio::test]
async fn schema_namespace_database_not_found() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/db/nonexistent/schemas"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// ---------------------------------------------------------------------------
// Bulk import endpoints
// ---------------------------------------------------------------------------

#[tokio::test]
async fn import_tsv_creates_nodes_and_edges() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/db/default/import/tsv"))
        .json(&json!({
            "data": "1\t2\n2\t3\n3\t1",
            "edge_type": "CONNECTS",
            "directed": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["nodes_created"], 3);
    assert_eq!(body["edges_created"], 3);
}

#[tokio::test]
async fn import_tsv_undirected_doubles_edges() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/db/default/import/tsv"))
        .json(&json!({
            "data": "1\t2\n2\t3",
            "edge_type": "LINK",
            "directed": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["nodes_created"], 3);
    assert_eq!(body["edges_created"], 4); // 2 edges x 2 directions
}

#[tokio::test]
async fn import_tsv_database_not_found() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/db/nonexistent/import/tsv"))
        .json(&json!({
            "data": "1\t2",
            "edge_type": "E",
            "directed": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// ---------------------------------------------------------------------------
// Compact endpoint
// ---------------------------------------------------------------------------

#[tokio::test]
async fn compact_feature_not_enabled_returns_400() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/admin/default/compact"))
        .send()
        .await
        .unwrap();

    let status = resp.status().as_u16();
    let expected = if cfg!(feature = "compact-store") {
        200
    } else {
        400
    };
    assert_eq!(status, expected, "unexpected status: {status}");
}

#[tokio::test]
async fn compact_not_found() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/admin/nonexistent/compact"))
        .send()
        .await
        .unwrap();

    // 404 (database not found) or 400 (feature not enabled).
    let status = resp.status().as_u16();
    assert!(
        (cfg!(feature = "compact-store") && status == 404)
            || (!cfg!(feature = "compact-store") && status == 400),
        "unexpected status: {status}"
    );
}

// ---------------------------------------------------------------------------
// Read-Only Mode
// ---------------------------------------------------------------------------

async fn spawn_read_only_server() -> String {
    let state = grafeo_server::AppState::new_in_memory_read_only(300);
    let mut app = grafeo_server::router(state);

    #[cfg(feature = "studio")]
    {
        app = grafeo_studio::router().merge(app);
    }

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

#[tokio::test]
async fn read_only_health_reports_status() {
    let base = spawn_read_only_server().await;
    let client = Client::new();

    let resp = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["read_only"], true);
}

#[tokio::test]
async fn read_only_queries_still_work() {
    let base = spawn_read_only_server().await;
    let client = Client::new();

    // Read queries should succeed
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n) RETURN count(n) AS cnt"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["columns"], json!(["cnt"]));
}

#[tokio::test]
async fn read_only_create_database_rejected() {
    let base = spawn_read_only_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/db"))
        .json(&json!({"name": "mydb"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "read_only");
}

#[tokio::test]
async fn read_only_delete_database_rejected() {
    let base = spawn_read_only_server().await;
    let client = Client::new();

    let resp = client
        .delete(format!("{base}/db/default"))
        .send()
        .await
        .unwrap();
    // Default DB deletion is rejected (either 403 for read-only or 400 for protected)
    let status = resp.status().as_u16();
    assert!(status == 403 || status == 400);
}

#[tokio::test]
async fn read_only_admin_create_index_rejected() {
    let base = spawn_read_only_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/admin/default/index"))
        .json(&json!({"type": "property", "property": "name"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "read_only");
}

#[tokio::test]
async fn read_only_admin_read_operations_allowed() {
    let base = spawn_read_only_server().await;
    let client = Client::new();

    // Stats should work
    let resp = client
        .get(format!("{base}/admin/default/stats"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // WAL status should work
    let resp = client
        .get(format!("{base}/admin/default/wal"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Memory usage should work
    let resp = client
        .get(format!("{base}/admin/default/memory"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Validate should work
    let resp = client
        .get(format!("{base}/admin/default/validate"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn read_only_system_resources_reports_status() {
    let base = spawn_read_only_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/system/resources"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["read_only"], true);
}

#[tokio::test]
async fn read_only_list_databases_works() {
    let base = spawn_read_only_server().await;
    let client = Client::new();

    let resp = client.get(format!("{base}/db")).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    let dbs = body["databases"].as_array().unwrap();
    assert!(!dbs.is_empty());
    assert!(dbs.iter().any(|d| d["name"] == "default"));
}

#[tokio::test]
async fn read_only_wal_checkpoint_rejected() {
    let base = spawn_read_only_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/admin/default/wal/checkpoint"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn read_only_named_graph_mutations_rejected() {
    let base = spawn_read_only_server().await;
    let client = Client::new();

    // Create graph should be rejected
    let resp = client
        .post(format!("{base}/db/default/graphs"))
        .json(&json!({"name": "mygraph"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    // List graphs should work
    let resp = client
        .get(format!("{base}/db/default/graphs"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

// ---------------------------------------------------------------------------
// End-to-End Scenario: Multi-Database Workflow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_multi_database_workflow() {
    let base = spawn_server().await;
    let client = Client::new();

    // 1. Start with only default database
    let resp = client.get(format!("{base}/db")).send().await.unwrap();
    let body: Value = resp.json().await.unwrap();
    let dbs = body["databases"].as_array().unwrap();
    assert_eq!(dbs.len(), 1);
    assert_eq!(dbs[0]["name"], "default");

    // 2. Create a secondary database
    let resp = client
        .post(format!("{base}/db"))
        .json(&json!({"name": "analytics"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 3. Insert data into both databases
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({
            "query": "INSERT (:User {name: 'Alice'})",
            "database": "default"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({
            "query": "INSERT (:Event {type: 'click', ts: 1234})",
            "database": "analytics"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 4. Verify data isolation: default has User, analytics has Event
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({
            "query": "MATCH (n:User) RETURN n.name AS name",
            "database": "default"
        }))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["rows"].as_array().unwrap().len(), 1);
    assert_eq!(body["rows"][0][0], "Alice");

    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({
            "query": "MATCH (n:User) RETURN count(n) AS cnt",
            "database": "analytics"
        }))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["rows"][0][0], 0);

    // 5. Check stats reflect inserted data
    let resp = client
        .get(format!("{base}/admin/default/stats"))
        .send()
        .await
        .unwrap();
    let stats: Value = resp.json().await.unwrap();
    assert!(stats["node_count"].as_u64().unwrap() >= 1);

    // 6. Delete analytics database
    let resp = client
        .delete(format!("{base}/db/analytics"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 7. Verify only default remains
    let resp = client.get(format!("{base}/db")).send().await.unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["databases"].as_array().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// End-to-End Scenario: Transaction Lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_transaction_with_rollback_and_retry() {
    let base = spawn_server().await;
    let client = Client::new();

    // 1. Begin transaction
    let resp = client
        .post(format!("{base}/tx/begin"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let session_id = body["session_id"].as_str().unwrap().to_string();
    assert_eq!(body["status"], "open");

    // 2. Insert data within transaction
    let resp = client
        .post(format!("{base}/tx/query"))
        .header("X-Session-Id", &session_id)
        .json(&json!({"query": "CREATE (:TxNode {val: 1})", "language": "cypher"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 3. Rollback
    let resp = client
        .post(format!("{base}/tx/rollback"))
        .header("X-Session-Id", &session_id)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "rolled_back");

    // 4. Verify data was not persisted
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n:TxNode) RETURN count(n) AS cnt"}))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["rows"][0][0], 0);

    // 5. Begin new transaction, insert, and commit
    let resp = client
        .post(format!("{base}/tx/begin"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let session_id = body["session_id"].as_str().unwrap().to_string();

    let resp = client
        .post(format!("{base}/tx/query"))
        .header("X-Session-Id", &session_id)
        .json(&json!({"query": "CREATE (:TxNode {val: 42})", "language": "cypher"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = client
        .post(format!("{base}/tx/commit"))
        .header("X-Session-Id", &session_id)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 6. Verify committed data
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n:TxNode) RETURN n.val AS val"}))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["rows"][0][0], 42);
}

// ---------------------------------------------------------------------------
// End-to-End Scenario: Multi-Language Dispatch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_multi_language_queries() {
    let base = spawn_server().await;
    let client = Client::new();

    // Setup: Insert data via GQL
    client
        .post(format!("{base}/query"))
        .json(&json!({"query": "INSERT (:Person {name: 'Bob', age: 30})-[:KNOWS]->(:Person {name: 'Carol', age: 25})"}))
        .send()
        .await
        .unwrap();

    // GQL via /query
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (p:Person) RETURN p.name AS name ORDER BY p.name"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["columns"], json!(["name"]));
    let names: Vec<&str> = body["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r[0].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Bob", "Carol"]);

    // Cypher via /cypher endpoint
    let resp = client
        .post(format!("{base}/cypher"))
        .json(&json!({"query": "MATCH (p:Person) RETURN count(p) AS cnt"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["rows"][0][0], 2);

    // Language override via /query with language field
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({
            "query": "MATCH (p:Person) RETURN count(p) AS cnt",
            "language": "cypher"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["rows"][0][0], 2);
}

// ---------------------------------------------------------------------------
// End-to-End Scenario: Named Graphs Lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_named_graphs_full_lifecycle() {
    let base = spawn_server().await;
    let client = Client::new();

    // 1. Initially no named graphs
    let resp = client
        .get(format!("{base}/db/default/graphs"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["graphs"].as_array().unwrap().is_empty());

    // 2. Create two named graphs
    let resp = client
        .post(format!("{base}/db/default/graphs"))
        .json(&json!({"name": "social"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["created"], true);

    let resp = client
        .post(format!("{base}/db/default/graphs"))
        .json(&json!({"name": "payments"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 3. List graphs
    let resp = client
        .get(format!("{base}/db/default/graphs"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let graphs = body["graphs"].as_array().unwrap();
    assert_eq!(graphs.len(), 2);

    // 4. Duplicate creation returns false
    let resp = client
        .post(format!("{base}/db/default/graphs"))
        .json(&json!({"name": "social"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["created"], false);

    // 5. Drop one graph
    let resp = client
        .delete(format!("{base}/db/default/graphs/payments"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["dropped"], true);

    // 6. Verify only one remains
    let resp = client
        .get(format!("{base}/db/default/graphs"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["graphs"].as_array().unwrap().len(), 1);
    assert_eq!(body["graphs"][0], "social");
}

// ---------------------------------------------------------------------------
// End-to-End Scenario: Batch Query Processing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_batch_query_workflow() {
    let base = spawn_server().await;
    let client = Client::new();

    // 1. Batch insert multiple nodes
    let resp = client
        .post(format!("{base}/batch"))
        .json(&json!({
            "queries": [
                {"query": "INSERT (:Item {name: 'A', price: 10})"},
                {"query": "INSERT (:Item {name: 'B', price: 20})"},
                {"query": "INSERT (:Item {name: 'C', price: 30})"}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 3);

    // 2. Batch query: all results succeed
    let resp = client
        .post(format!("{base}/batch"))
        .json(&json!({
            "queries": [
                {"query": "MATCH (i:Item) RETURN count(i) AS cnt"},
                {"query": "MATCH (i:Item) RETURN sum(i.price) AS total"}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let results = body["results"].as_array().unwrap();
    assert_eq!(results[0]["rows"][0][0], 3);
    assert_eq!(results[1]["rows"][0][0], 60);
}

// ---------------------------------------------------------------------------
// End-to-End Scenario: WebSocket Streaming
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_websocket_multiple_queries() {
    let base = spawn_server().await;
    let ws_url = base.replace("http://", "ws://");

    // Setup data first
    let client = Client::new();
    client
        .post(format!("{base}/query"))
        .json(
            &json!({"query": "INSERT (:WsNode {val: 1}), (:WsNode {val: 2}), (:WsNode {val: 3})"}),
        )
        .send()
        .await
        .unwrap();

    // Connect via WebSocket
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("{ws_url}/ws"))
        .await
        .unwrap();

    // Send query with ID
    let msg = json!({
        "type": "query",
        "id": "q1",
        "query": "MATCH (n:WsNode) RETURN count(n) AS cnt"
    });
    ws.send(tungstenite::Message::Text(msg.to_string().into()))
        .await
        .unwrap();

    // Receive result
    let response = ws.next().await.unwrap().unwrap();
    let body: Value = serde_json::from_str(response.to_text().unwrap()).unwrap();
    assert_eq!(body["type"], "result");
    assert_eq!(body["id"], "q1");
    assert_eq!(body["rows"][0][0], 3);

    // Send another query with different ID
    let msg = json!({
        "type": "query",
        "id": "q2",
        "query": "MATCH (n:WsNode) RETURN sum(n.val) AS total"
    });
    ws.send(tungstenite::Message::Text(msg.to_string().into()))
        .await
        .unwrap();

    let response = ws.next().await.unwrap().unwrap();
    let body: Value = serde_json::from_str(response.to_text().unwrap()).unwrap();
    assert_eq!(body["type"], "result");
    assert_eq!(body["id"], "q2");
    assert_eq!(body["rows"][0][0], 6);

    // Ping/Pong
    ws.send(tungstenite::Message::Text(
        json!({"type": "ping"}).to_string().into(),
    ))
    .await
    .unwrap();

    let response = ws.next().await.unwrap().unwrap();
    let body: Value = serde_json::from_str(response.to_text().unwrap()).unwrap();
    assert_eq!(body["type"], "pong");
}

// ---------------------------------------------------------------------------
// End-to-End Scenario: Graph Algorithms
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_graph_algorithms() {
    let base = spawn_server().await;
    let client = Client::new();

    // Build a small graph
    client
        .post(format!("{base}/query"))
        .json(&json!({"query": "INSERT (:City {name: 'NYC'})-[:ROAD {dist: 200}]->(:City {name: 'DC'})-[:ROAD {dist: 100}]->(:City {name: 'Philly'})"}))
        .send()
        .await
        .unwrap();

    // Run WCC (weakly connected components) via Cypher
    let resp = client
        .post(format!("{base}/cypher"))
        .json(&json!({
            "query": "CALL grafeo.connected_components() YIELD node_id, component_id RETURN count(*) AS total"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["rows"][0][0].as_u64().unwrap() >= 3);

    // Run PageRank via Cypher
    let resp = client
        .post(format!("{base}/cypher"))
        .json(&json!({"query": "CALL grafeo.pagerank() YIELD node_id, score RETURN count(*) AS nodes"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["rows"][0][0].as_u64().unwrap() >= 3);
}

// ---------------------------------------------------------------------------
// End-to-End Scenario: Database Creation with Options
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_database_with_full_options() {
    let base = spawn_server().await;
    let client = Client::new();

    // Create database with custom options
    let resp = client
        .post(format!("{base}/db"))
        .json(&json!({
            "name": "custom",
            "database_type": "Lpg",
            "storage_mode": "InMemory",
            "options": {
                "memory_limit_bytes": 268_435_456,
                "backward_edges": false,
                "threads": 2
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Verify database info reflects options
    let resp = client
        .get(format!("{base}/db/custom"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "custom");
    assert_eq!(body["backward_edges"], false);
    assert_eq!(body["threads"], 2);

    // Insert and query on the custom database
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({
            "query": "INSERT (:Node {id: 1})-[:LINK]->(:Node {id: 2})",
            "database": "custom"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({
            "query": "MATCH (a)-[:LINK]->(b) RETURN a.id AS src, b.id AS dst",
            "database": "custom"
        }))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["rows"][0][0], 1);
    assert_eq!(body["rows"][0][1], 2);

    // Cleanup
    client
        .delete(format!("{base}/db/custom"))
        .send()
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// End-to-End Scenario: Cache Management
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_cache_stats_and_clear() {
    let base = spawn_server().await;
    let client = Client::new();

    // Run a few queries to populate the cache
    for _ in 0..3 {
        client
            .post(format!("{base}/query"))
            .json(&json!({"query": "MATCH (n) RETURN count(n) AS cnt"}))
            .send()
            .await
            .unwrap();
    }

    // Check cache stats
    let resp = client
        .get(format!("{base}/admin/default/cache"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    // Verify the response structure contains expected fields
    assert!(body["parsed_size"].is_u64());
    assert!(body["optimized_size"].is_u64());
    assert!(body["parsed_hits"].is_u64());
    assert!(body["parsed_misses"].is_u64());

    // Clear cache
    let resp = client
        .post(format!("{base}/admin/default/cache/clear"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Verify cache was cleared (size drops to 0)
    let resp = client
        .get(format!("{base}/admin/default/cache"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["parsed_size"], 0);
    assert_eq!(body["optimized_size"], 0);
}

// ---------------------------------------------------------------------------
// End-to-End Scenario: Metrics Tracking
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_metrics_reflect_activity() {
    let base = spawn_server().await;
    let client = Client::new();

    // Run queries in different languages
    client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n) RETURN count(n) AS cnt"}))
        .send()
        .await
        .unwrap();

    client
        .post(format!("{base}/cypher"))
        .json(&json!({"query": "MATCH (n) RETURN count(n) AS cnt"}))
        .send()
        .await
        .unwrap();

    // Trigger an error
    client
        .post(format!("{base}/query"))
        .json(&json!({"query": "THIS IS INVALID SYNTAX"}))
        .send()
        .await
        .unwrap();

    // Check metrics
    let resp = client.get(format!("{base}/metrics")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();

    // Server-level metrics
    assert!(body.contains("grafeo_databases_total 1"));
    assert!(body.contains("grafeo_active_sessions_total 0"));
    assert!(body.contains("grafeo_uptime_seconds"));

    // Per-language query counters
    assert!(body.contains("grafeo_queries_total{language=\"gql\"}"));
    assert!(body.contains("grafeo_queries_total{language=\"cypher\"}"));

    // Error counter
    assert!(body.contains("grafeo_query_errors_total{language=\"gql\"}"));
}

// ===========================================================================
// Offline-first sync: HTTP round-trip (v0.4.9)
// ===========================================================================

/// Creates an in-memory AppState with CDC enabled (required for sync).
///
/// Sync endpoints depend on CDC to record change events. In production this
/// is activated by replication mode; tests must enable it explicitly.
#[cfg(feature = "sync")]
fn sync_state() -> grafeo_server::AppState {
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
        #[cfg(feature = "auth")]
        token_store_path: None,
        #[cfg(feature = "replication")]
        replication_mode: grafeo_service::replication::ReplicationMode::Primary,
        backup_dir: None,
        backup_retention: None,
    };
    let service = grafeo_service::ServiceState::new(&config);
    grafeo_server::AppState::new(
        service,
        vec![],
        grafeo_service::types::EnabledFeatures::default(),
    )
}

/// Boots a server from a pre-seeded state, pulls its changefeed via HTTP,
/// then pushes those changes to a second fresh server and verifies the result.
///
/// This test exercises the full client workflow:
///   1. Seed nodes via the direct API (populates CDC log)
///   2. Pull `GET /db/default/changes?since=0`
///   3. Convert `ChangeEventDto` → `SyncChangeRequest`
///   4. Push `POST /db/default/sync` to a second server
///   5. Verify `applied`, `id_mappings`, and lack of conflicts
#[cfg(feature = "sync")]
#[tokio::test]
async fn sync_round_trip_two_databases() {
    // --- Server A: seed 3 Person nodes via direct API ---
    let state_a = sync_state();
    {
        let entry = state_a.databases().get("default").unwrap();
        entry.db().create_node(&["Person"]);
        entry.db().create_node(&["Person"]);
        entry.db().create_node(&["Person"]);
    }
    let base_a = spawn_server_from_state(state_a).await;

    // --- Pull changefeed from server A ---
    let client = Client::new();
    let pull_resp = client
        .get(format!("{base_a}/db/default/changes?since=0"))
        .send()
        .await
        .unwrap();
    assert_eq!(pull_resp.status(), 200);

    let pull_body: Value = pull_resp.json().await.unwrap();
    let changes = pull_body["changes"].as_array().unwrap();

    // 3 node create events
    assert_eq!(changes.len(), 3);
    assert!(changes.iter().all(|e| e["kind"] == "create"));
    assert!(changes.iter().all(|e| e["entity_type"] == "node"));
    assert!(changes.iter().all(|e| {
        e["labels"]
            .as_array()
            .is_some_and(|ls| ls.contains(&Value::String("Person".into())))
    }));

    let server_epoch = pull_body["server_epoch"].as_u64().unwrap();

    // --- Convert ChangeEventDto → SyncChangeRequest (node creates) ---
    let sync_changes: Vec<Value> = changes
        .iter()
        .map(|e| {
            json!({
                "kind":        "create",
                "entity_type": "node",
                "labels":      e["labels"],
                "after":       e["after"],
                "timestamp":   e["timestamp"],
            })
        })
        .collect();

    let sync_req = json!({
        "client_id":       "test-round-trip",
        "last_seen_epoch": server_epoch,
        "changes":         sync_changes,
    });

    // --- Apply to server B (fresh, empty, CDC enabled) ---
    let base_b = spawn_server_from_state(sync_state()).await;
    let apply_resp = client
        .post(format!("{base_b}/db/default/sync"))
        .json(&sync_req)
        .send()
        .await
        .unwrap();
    assert_eq!(apply_resp.status(), 200);

    let apply_body: Value = apply_resp.json().await.unwrap();

    // All 3 creates applied, no conflicts
    assert_eq!(apply_body["applied"], 3);
    assert_eq!(apply_body["skipped"], 0);
    assert!(apply_body["conflicts"].as_array().unwrap().is_empty());

    // Server B assigned a new ID for each create
    let mappings = apply_body["id_mappings"].as_array().unwrap();
    assert_eq!(mappings.len(), 3);
    for (idx, mapping) in mappings.iter().enumerate() {
        assert_eq!(mapping["request_index"].as_u64().unwrap(), idx as u64);
        assert!(mapping["server_id"].as_u64().is_some());
        assert_ne!(mapping["server_id"].as_u64().unwrap(), u64::MAX);
    }

    // server_epoch in the apply response should be non-zero (writes advanced epoch)
    assert!(apply_body["server_epoch"].as_u64().is_some());
}

/// Sync: edge creates require remapping src/dst IDs via id_mappings before pushing.
///
/// Creates two Person nodes and a KNOWS edge on server A, pulls the changefeed,
/// pushes the node creates to server B, remaps the edge endpoints using the
/// returned id_mappings, then pushes the edge create and verifies it lands.
#[cfg(feature = "sync")]
#[tokio::test]
async fn sync_with_edge_creates() {
    let state_a = sync_state();
    let (alix_raw, gus_raw) = {
        let entry = state_a.databases().get("default").unwrap();
        let alix = entry.db().create_node(&["Person"]);
        let gus = entry.db().create_node(&["Person"]);
        entry.db().create_edge(alix, gus, "KNOWS");
        (alix.as_u64(), gus.as_u64())
    };
    let base_a = spawn_server_from_state(state_a).await;

    let client = Client::new();
    let pull: Value = client
        .get(format!("{base_a}/db/default/changes?since=0"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let changes = pull["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 3, "expected 2 node + 1 edge create events");

    let node_changes: Vec<&Value> = changes
        .iter()
        .filter(|e| e["entity_type"] == "node")
        .collect();
    let edge_changes: Vec<&Value> = changes
        .iter()
        .filter(|e| e["entity_type"] == "edge")
        .collect();
    assert_eq!(node_changes.len(), 2);
    assert_eq!(edge_changes.len(), 1);

    // Push node creates to server B (CDC enabled); collect id_mappings.
    let base_b = spawn_server_from_state(sync_state()).await;
    let node_req = json!({
        "client_id": "test-edge-sync",
        "last_seen_epoch": 0u64,
        "changes": node_changes.iter().map(|e| json!({
            "kind": "create",
            "entity_type": "node",
            "labels": e["labels"],
            "after": e["after"],
            "timestamp": e["timestamp"],
        })).collect::<Vec<_>>(),
    });
    let node_resp: Value = client
        .post(format!("{base_b}/db/default/sync"))
        .json(&node_req)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(node_resp["applied"], 2);
    let node_mappings = node_resp["id_mappings"].as_array().unwrap();
    assert_eq!(node_mappings.len(), 2);

    // Build server-A-ID -> server-B-ID map.
    let a_node_ids: Vec<u64> = node_changes
        .iter()
        .map(|e| e["id"].as_u64().unwrap())
        .collect();
    let mut a_to_b: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
    for m in node_mappings {
        let idx = m["request_index"].as_u64().unwrap() as usize;
        a_to_b.insert(a_node_ids[idx], m["server_id"].as_u64().unwrap());
    }
    assert!(a_to_b.contains_key(&alix_raw));
    assert!(a_to_b.contains_key(&gus_raw));

    // Remap edge endpoints and push.
    let edge = edge_changes[0];
    let src_b = a_to_b[&edge["src_id"].as_u64().unwrap()];
    let dst_b = a_to_b[&edge["dst_id"].as_u64().unwrap()];
    let edge_req = json!({
        "client_id": "test-edge-sync",
        "last_seen_epoch": node_resp["server_epoch"],
        "changes": [{
            "kind": "create",
            "entity_type": "edge",
            "edge_type": edge["edge_type"],
            "src_id": src_b,
            "dst_id": dst_b,
            "timestamp": edge["timestamp"],
        }],
    });
    let edge_resp: Value = client
        .post(format!("{base_b}/db/default/sync"))
        .json(&edge_req)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(edge_resp["applied"], 1);
    assert!(edge_resp["conflicts"].as_array().unwrap().is_empty());
    let edge_mappings = edge_resp["id_mappings"].as_array().unwrap();
    assert_eq!(edge_mappings.len(), 1);
    assert!(edge_mappings[0]["server_id"].as_u64().is_some());
}

/// Sync: property updates and node deletes are applied correctly.
///
/// Seeds two nodes on server B directly. Pushes an update (property delta) for
/// the first and a delete for the second using future-dated timestamps to avoid
/// LWW conflicts. Verifies both are applied with no conflicts.
#[cfg(feature = "sync")]
#[tokio::test]
async fn sync_updates_and_deletes() {
    let state_b = sync_state();
    let (n1, n2) = {
        let entry = state_b.databases().get("default").unwrap();
        let n1 = entry.db().create_node(&["Device"]).as_u64();
        let n2 = entry.db().create_node(&["Device"]).as_u64();
        (n1, n2)
    };
    let base_b = spawn_server_from_state(state_b).await;

    // Use a timestamp well in the future so LWW never fires.
    let future_ts = u64::MAX / 2;

    let client = Client::new();
    let sync_req = json!({
        "client_id": "test-upd-del",
        "last_seen_epoch": 0u64,
        "changes": [
            {
                "kind": "update",
                "entity_type": "node",
                "id": n1,
                "after": {"status": {"String": "active"}},
                "timestamp": future_ts,
            },
            {
                "kind": "delete",
                "entity_type": "node",
                "id": n2,
                "timestamp": future_ts,
            },
        ],
    });
    let resp: Value = client
        .post(format!("{base_b}/db/default/sync"))
        .json(&sync_req)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp["applied"], 2);
    assert_eq!(resp["skipped"], 0);
    assert!(resp["conflicts"].as_array().unwrap().is_empty());
    // No creates, so no id_mappings.
    assert!(resp["id_mappings"].as_array().unwrap().is_empty());
}

/// Sync: LWW conflict detection — stale client update is rejected.
///
/// A node is created directly on the server (CDC records it with the current
/// wall-clock time T). Pushing an update with timestamp=1 (which is less than T)
/// should be detected as "server_newer" and appear in the conflicts list.
#[cfg(feature = "sync")]
#[tokio::test]
async fn sync_lww_conflict_detection() {
    let state = sync_state();
    let node_id = {
        let entry = state.databases().get("default").unwrap();
        entry.db().create_node(&["Person"]).as_u64()
    };
    let base = spawn_server_from_state(state).await;

    let client = Client::new();
    let sync_req = json!({
        "client_id": "test-lww",
        "last_seen_epoch": 0u64,
        "changes": [{
            "kind": "update",
            "entity_type": "node",
            "id": node_id,
            "after": {"name": {"String": "stale-value"}},
            "timestamp": 1u64,   // deliberatley older than the server's CDC record
        }],
    });
    let resp: Value = client
        .post(format!("{base}/db/default/sync"))
        .json(&sync_req)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp["applied"], 0);
    assert_eq!(resp["skipped"], 1);
    let conflicts = resp["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0]["request_index"], 0);
    assert_eq!(conflicts[0]["reason"], "server_newer");
}

/// Sync: the `limit` query parameter truncates the changefeed response.
///
/// Seeds 7 nodes, then verifies that `limit=5` returns exactly 5 events while
/// `limit=20` returns all 7. Also verifies that polling with `since` equal to
/// the current server epoch returns an empty changefeed (caught-up state).
#[cfg(feature = "sync")]
#[tokio::test]
async fn sync_limit_truncation() {
    let state = sync_state();
    {
        let entry = state.databases().get("default").unwrap();
        for _ in 0..7 {
            entry.db().create_node(&["Item"]);
        }
    }
    let base = spawn_server_from_state(state).await;
    let client = Client::new();

    // Truncated pull.
    let page: Value = client
        .get(format!("{base}/db/default/changes?since=0&limit=5"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page["changes"].as_array().unwrap().len(), 5);

    // Full pull.
    let full: Value = client
        .get(format!("{base}/db/default/changes?since=0&limit=20"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(full["changes"].as_array().unwrap().len(), 7);

    // Caught-up: since = server_epoch + 1 returns empty.
    let server_epoch = full["server_epoch"].as_u64().unwrap();
    let caught_up: Value = client
        .get(format!(
            "{base}/db/default/changes?since={}&limit=20",
            server_epoch + 1
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(caught_up["changes"].as_array().unwrap().is_empty());
}

/// Sync: missing required fields produce descriptive conflict reasons.
///
/// Verifies the three structural validation errors: `update_missing_id`,
/// `delete_missing_id`, and `edge_create_missing_src_dst_or_type`.
#[cfg(feature = "sync")]
#[tokio::test]
async fn sync_validation_errors() {
    let base = spawn_server_from_state(sync_state()).await;
    let client = Client::new();

    let sync_req = json!({
        "client_id": "test-validation",
        "last_seen_epoch": 0u64,
        "changes": [
            // update without id
            {
                "kind": "update",
                "entity_type": "node",
                "after": {"name": {"String": "x"}},
                "timestamp": 1u64,
            },
            // delete without id
            {
                "kind": "delete",
                "entity_type": "node",
                "timestamp": 1u64,
            },
            // edge create without src/dst/type
            {
                "kind": "create",
                "entity_type": "edge",
                "timestamp": 1u64,
            },
        ],
    });
    let resp: Value = client
        .post(format!("{base}/db/default/sync"))
        .json(&sync_req)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp["applied"], 0);
    let conflicts = resp["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 3);

    let reasons: Vec<&str> = conflicts
        .iter()
        .map(|c| c["reason"].as_str().unwrap())
        .collect();
    assert!(reasons.contains(&"update_missing_id"));
    assert!(reasons.contains(&"delete_missing_id"));
    assert!(reasons.contains(&"edge_create_missing_src_dst_or_type"));
}

// ---------------------------------------------------------------------------
// WebSocket error paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn websocket_query_bad_syntax_returns_error() {
    let base = spawn_server().await;
    let ws_url = base.replace("http://", "ws://") + "/ws";

    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();

    // Send a query with invalid GQL — should produce a "bad_request" error frame.
    ws.send(tungstenite::Message::Text(
        json!({
            "type": "query",
            "id": "err1",
            "query": "NOT VALID SYNTAX !!!"
        })
        .to_string()
        .into(),
    ))
    .await
    .unwrap();

    let reply = ws.next().await.unwrap().unwrap();
    let body: Value = serde_json::from_str(reply.to_text().unwrap()).unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["id"], "err1");
    assert_eq!(body["error"], "bad_request");
    assert!(
        body["detail"].is_string(),
        "detail should carry the parse error"
    );
}

#[tokio::test]
async fn websocket_query_nonexistent_database_returns_not_found() {
    let base = spawn_server().await;
    let ws_url = base.replace("http://", "ws://") + "/ws";

    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();

    ws.send(tungstenite::Message::Text(
        json!({
            "type": "query",
            "id": "db-miss",
            "query": "MATCH (n) RETURN n",
            "database": "no_such_db"
        })
        .to_string()
        .into(),
    ))
    .await
    .unwrap();

    let reply = ws.next().await.unwrap().unwrap();
    let body: Value = serde_json::from_str(reply.to_text().unwrap()).unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["id"], "db-miss");
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn websocket_query_without_id_omits_id_in_response() {
    let base = spawn_server().await;
    let ws_url = base.replace("http://", "ws://") + "/ws";

    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();

    // No "id" field in the message.
    ws.send(tungstenite::Message::Text(
        json!({
            "type": "query",
            "query": "MATCH (n) RETURN count(n)"
        })
        .to_string()
        .into(),
    ))
    .await
    .unwrap();

    let reply = ws.next().await.unwrap().unwrap();
    let body: Value = serde_json::from_str(reply.to_text().unwrap()).unwrap();
    assert_eq!(body["type"], "result");
    assert!(
        body["id"].is_null(),
        "id should be absent when not provided"
    );
}

#[tokio::test]
async fn websocket_query_with_language_field() {
    let base = spawn_server().await;
    let ws_url = base.replace("http://", "ws://") + "/ws";

    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();

    ws.send(tungstenite::Message::Text(
        json!({
            "type": "query",
            "id": "cyq",
            "query": "MATCH (n) RETURN count(n) AS c",
            "language": "cypher"
        })
        .to_string()
        .into(),
    ))
    .await
    .unwrap();

    let reply = ws.next().await.unwrap().unwrap();
    let body: Value = serde_json::from_str(reply.to_text().unwrap()).unwrap();
    assert_eq!(body["type"], "result");
    assert_eq!(body["id"], "cyq");
    assert!(body["rows"].is_array());
}

// ---------------------------------------------------------------------------
// Rate limit: window reset
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rate_limit_resets_after_window_expires() {
    // 2-request window that expires after 300 ms.
    let state = grafeo_server::AppState::new_in_memory_with_rate_limit(
        300,
        2,
        std::time::Duration::from_millis(300),
    );
    let app = grafeo_server::router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let base = format!("http://{addr}");
    let client = Client::new();

    // Exhaust the window.
    for _ in 0..2 {
        let r = client.get(format!("{base}/health")).send().await.unwrap();
        assert_eq!(r.status(), 200);
    }
    let r = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(r.status(), 429, "third request should be rate-limited");

    // Wait for the window to expire then verify the counter resets.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    let r = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(
        r.status(),
        200,
        "first request in new window should succeed"
    );
}

// ---------------------------------------------------------------------------
// Transaction error paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tx_begin_on_nonexistent_database_returns_404() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/tx/begin"))
        .json(&json!({"database": "ghost_db"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn tx_query_with_expired_session_returns_404() {
    // Use a TTL-1 server so the session expires almost immediately.
    let state = grafeo_server::AppState::new_in_memory(1);
    let base = spawn_server_from_state(state).await;
    let client = Client::new();

    // Open a session.
    let resp = client
        .post(format!("{base}/tx/begin"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let session_id = body["session_id"].as_str().unwrap().to_string();

    // Wait for the 1-second TTL to expire.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // A query against the expired session should return 404.
    let resp = client
        .post(format!("{base}/tx/query"))
        .header("X-Session-Id", &session_id)
        .json(&json!({"query": "MATCH (n) RETURN n"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "session_not_found");
}

// ---------------------------------------------------------------------------
// Database info: 404 on missing database
// ---------------------------------------------------------------------------

#[tokio::test]
async fn database_info_not_found_returns_404() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/db/no_such_database"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn database_stats_not_found_returns_404() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/db/no_such_database/stats"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn database_schema_not_found_returns_404() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/db/no_such_database/schema"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// ---------------------------------------------------------------------------
// Query on nonexistent database returns 404
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_on_nonexistent_database_returns_404() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({
            "query": "MATCH (n) RETURN n",
            "database": "ghost_db"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "not_found");
}

#[cfg(feature = "bolt")]
#[tokio::test]
async fn bolt_route_returns_routing_table() {
    let (_http, bolt_addr) = spawn_server_with_bolt().await;

    // Exercises GrafeoBackend::route via a low-level ROUTE message.
    let mut session = boltr::client::BoltSession::connect(bolt_addr)
        .await
        .unwrap();

    let conn = session.connection();
    conn.send(&boltr::message::request::ClientMessage::Route {
        routing: boltr::types::BoltDict::new(),
        bookmarks: vec![],
        extra: boltr::types::BoltDict::new(),
    })
    .await
    .expect("ROUTE send failed");

    let response = conn.recv().await.expect("ROUTE recv failed");
    match response {
        boltr::message::response::ServerMessage::Success { metadata } => {
            let rt = metadata.get("rt").expect("response should contain 'rt'");
            let boltr::types::BoltValue::Dict(rt_dict) = rt else {
                panic!("'rt' should be a Dict");
            };
            assert!(
                rt_dict.contains_key("servers"),
                "routing table needs servers"
            );
            assert!(rt_dict.contains_key("ttl"), "routing table needs ttl");
        }
        other => panic!("expected SUCCESS after ROUTE, got {other:?}"),
    }

    session.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Backup & Restore
// ---------------------------------------------------------------------------

/// Boots a server with backup configured, returning (base_url, backup_tempdir).
/// The TempDir must be kept alive for the duration of the test.
async fn spawn_server_with_backup() -> (String, TempDir) {
    let backup_dir = TempDir::new().unwrap();
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
        #[cfg(feature = "auth")]
        token_store_path: None,
        #[cfg(feature = "replication")]
        replication_mode: grafeo_service::replication::ReplicationMode::Standalone,
        backup_dir: Some(backup_dir.path().to_str().unwrap().to_string()),
        backup_retention: None,
    };
    let service = grafeo_service::ServiceState::new(&config);
    let state = grafeo_server::AppState::new(
        service,
        vec![],
        grafeo_service::types::EnabledFeatures::default(),
    );
    let base = spawn_server_from_state(state).await;
    (base, backup_dir)
}

#[tokio::test]
async fn backup_requires_configuration() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/admin/default/backup"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn backup_create_and_list() {
    let (base, _dir) = spawn_server_with_backup().await;
    let client = Client::new();

    // Create a backup
    let resp = client
        .post(format!("{base}/admin/default/backup"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["database"], "default");
    assert!(body["filename"].as_str().unwrap().ends_with(".grafeo"));
    assert!(body["size_bytes"].as_u64().unwrap() > 0);

    // List backups for this database
    let resp = client
        .get(format!("{base}/admin/default/backups"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let backups: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(backups.len(), 1);
    assert_eq!(backups[0]["database"], "default");
}

#[tokio::test]
async fn backup_list_all() {
    let (base, _dir) = spawn_server_with_backup().await;
    let client = Client::new();

    client
        .post(format!("{base}/admin/default/backup"))
        .send()
        .await
        .unwrap();

    let resp = client.get(format!("{base}/backups")).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let backups: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(backups.len(), 1);
}

#[tokio::test]
async fn backup_not_found_database() {
    let (base, _dir) = spawn_server_with_backup().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/admin/nonexistent/backup"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn backup_delete() {
    let (base, _dir) = spawn_server_with_backup().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/admin/default/backup"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let filename = body["filename"].as_str().unwrap();

    let resp = client
        .delete(format!("{base}/backups/{filename}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = client
        .get(format!("{base}/admin/default/backups"))
        .send()
        .await
        .unwrap();
    let backups: Vec<Value> = resp.json().await.unwrap();
    assert!(backups.is_empty());
}

#[tokio::test]
async fn backup_delete_not_found() {
    let (base, _dir) = spawn_server_with_backup().await;
    let client = Client::new();

    let resp = client
        .delete(format!("{base}/backups/nonexistent.grafeo"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn backup_download() {
    let (base, _dir) = spawn_server_with_backup().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/admin/default/backup"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let filename = body["filename"].as_str().unwrap();
    let size = body["size_bytes"].as_u64().unwrap();

    let resp = client
        .get(format!("{base}/backups/download/{filename}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-disposition")
            .unwrap()
            .to_str()
            .unwrap(),
        &format!("attachment; filename=\"{filename}\"")
    );

    let bytes = resp.bytes().await.unwrap();
    assert_eq!(bytes.len() as u64, size);
}

#[tokio::test]
async fn backup_download_not_found() {
    let (base, _dir) = spawn_server_with_backup().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/backups/download/nonexistent.grafeo"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn restore_requires_backup_config() {
    let base = spawn_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/admin/default/restore"))
        .json(&json!({ "backup": "something.grafeo" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn restore_in_memory_rejected() {
    let (base, _dir) = spawn_server_with_backup().await;
    let client = Client::new();

    // Create a backup first so the file exists
    let resp = client
        .post(format!("{base}/admin/default/backup"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let filename = body["filename"].as_str().unwrap();

    // Restore should fail because the default db is in-memory
    let resp = client
        .post(format!("{base}/admin/default/restore"))
        .json(&json!({ "backup": filename }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

/// Boots a server with persistent storage and backup configured.
/// Returns (base_url, data_tempdir, backup_tempdir).
async fn spawn_server_persistent_backup() -> (String, TempDir, TempDir) {
    let data_dir = TempDir::new().unwrap();
    let backup_dir = TempDir::new().unwrap();
    let config = grafeo_service::ServiceConfig {
        data_dir: Some(data_dir.path().to_str().unwrap().to_string()),
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
        backup_dir: Some(backup_dir.path().to_str().unwrap().to_string()),
        backup_retention: None,
    };
    let service = grafeo_service::ServiceState::new(&config);
    let state = grafeo_server::AppState::new(
        service,
        vec![],
        grafeo_service::types::EnabledFeatures::default(),
    );
    let base = spawn_server_from_state(state).await;
    (base, data_dir, backup_dir)
}

/// Same as above but with retention configured.
async fn spawn_server_persistent_backup_with_retention(keep: usize) -> (String, TempDir, TempDir) {
    let data_dir = TempDir::new().unwrap();
    let backup_dir = TempDir::new().unwrap();
    let config = grafeo_service::ServiceConfig {
        data_dir: Some(data_dir.path().to_str().unwrap().to_string()),
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
        backup_dir: Some(backup_dir.path().to_str().unwrap().to_string()),
        backup_retention: Some(keep),
    };
    let service = grafeo_service::ServiceState::new(&config);
    let state = grafeo_server::AppState::new(
        service,
        vec![],
        grafeo_service::types::EnabledFeatures::default(),
    );
    let base = spawn_server_from_state(state).await;
    (base, data_dir, backup_dir)
}

/// Helper: create N nodes on a database via GQL.
async fn seed_nodes(client: &Client, base: &str, db: &str, count: usize) {
    for i in 0..count {
        client
            .post(format!("{base}/query"))
            .json(&json!({
                "query": format!("CREATE (:Item {{idx: {i}}})"),
                "database": db,
            }))
            .send()
            .await
            .unwrap();
    }
}

/// Helper: get node count for a database.
async fn db_node_count(client: &Client, base: &str, db: &str) -> u64 {
    let resp: Value = client
        .get(format!("{base}/db/{db}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    resp["node_count"].as_u64().unwrap()
}

#[tokio::test]
async fn restore_data_rollback() {
    let (base, _data, _backup) = spawn_server_persistent_backup().await;
    let client = Client::new();

    // Seed 10 nodes
    seed_nodes(&client, &base, "default", 10).await;
    assert_eq!(db_node_count(&client, &base, "default").await, 10);

    // Backup
    let resp: Value = client
        .post(format!("{base}/admin/default/backup"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let filename = resp["filename"].as_str().unwrap().to_string();
    assert_eq!(resp["node_count"], 10);

    // Add more nodes
    seed_nodes(&client, &base, "default", 5).await;
    assert_eq!(db_node_count(&client, &base, "default").await, 15);

    // Restore — should roll back to 10
    let resp = client
        .post(format!("{base}/admin/default/restore"))
        .json(&json!({ "backup": filename }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(db_node_count(&client, &base, "default").await, 10);
}

#[tokio::test]
async fn restore_creates_safety_backup() {
    let (base, _data, _backup) = spawn_server_persistent_backup().await;
    let client = Client::new();

    // Backup
    let resp: Value = client
        .post(format!("{base}/admin/default/backup"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let filename = resp["filename"].as_str().unwrap().to_string();

    // Restore
    client
        .post(format!("{base}/admin/default/restore"))
        .json(&json!({ "backup": filename }))
        .send()
        .await
        .unwrap();

    // Should now have 2 backups: original + safety
    let backups: Vec<Value> = client
        .get(format!("{base}/admin/default/backups"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(backups.len(), 2);
}

#[tokio::test]
async fn backup_retention_enforced() {
    let (base, _data, _backup) = spawn_server_persistent_backup_with_retention(3).await;
    let client = Client::new();

    // Create 5 backups
    for _ in 0..5 {
        client
            .post(format!("{base}/admin/default/backup"))
            .send()
            .await
            .unwrap();
    }

    let backups: Vec<Value> = client
        .get(format!("{base}/admin/default/backups"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        backups.len() <= 3,
        "retention should keep at most 3, got {}",
        backups.len()
    );
}

#[tokio::test]
async fn backup_download_integrity() {
    let (base, _data, backup_dir) = spawn_server_persistent_backup().await;
    let client = Client::new();

    seed_nodes(&client, &base, "default", 10).await;

    // Backup and download
    let resp: Value = client
        .post(format!("{base}/admin/default/backup"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let filename = resp["filename"].as_str().unwrap().to_string();

    let bytes = client
        .get(format!("{base}/backups/download/{filename}"))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert!(!bytes.is_empty());

    // Write downloaded bytes as a new file in the backup dir
    let copy_path = backup_dir.path().join("downloaded_copy.grafeo");
    std::fs::write(&copy_path, &bytes).unwrap();

    // Mutate
    seed_nodes(&client, &base, "default", 5).await;
    assert_eq!(db_node_count(&client, &base, "default").await, 15);

    // Restore from the downloaded copy
    let resp = client
        .post(format!("{base}/admin/default/restore"))
        .json(&json!({ "backup": "downloaded_copy.grafeo" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(db_node_count(&client, &base, "default").await, 10);
}

#[tokio::test]
async fn restore_corrupt_backup_recovers() {
    let (base, _data, backup_dir) = spawn_server_persistent_backup().await;
    let client = Client::new();

    seed_nodes(&client, &base, "default", 10).await;

    // Write a corrupt file
    std::fs::write(backup_dir.path().join("corrupt.grafeo"), b"garbage").unwrap();

    let resp = client
        .post(format!("{base}/admin/default/restore"))
        .json(&json!({ "backup": "corrupt.grafeo" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 500);

    // Database should still be functional with original data
    assert_eq!(db_node_count(&client, &base, "default").await, 10);
}

#[tokio::test]
async fn restore_concurrent_conflict() {
    let (base, _data, _backup) = spawn_server_persistent_backup().await;
    let client = Client::new();

    let resp: Value = client
        .post(format!("{base}/admin/default/backup"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let filename = resp["filename"].as_str().unwrap().to_string();

    // Fire two restores concurrently
    let base2 = base.clone();
    let filename2 = filename.clone();
    let (r1, r2) = tokio::join!(
        async {
            reqwest::Client::new()
                .post(format!("{base}/admin/default/restore"))
                .json(&json!({ "backup": filename }))
                .send()
                .await
                .unwrap()
        },
        async {
            reqwest::Client::new()
                .post(format!("{base2}/admin/default/restore"))
                .json(&json!({ "backup": filename2 }))
                .send()
                .await
                .unwrap()
        },
    );

    let statuses = [r1.status().as_u16(), r2.status().as_u16()];
    let mut sorted = statuses;
    sorted.sort_unstable();

    // One should succeed (200), the other should get conflict (409)
    // or internal error (500) if the safety backup timestamp collided.
    // At minimum, one must succeed.
    assert!(
        sorted[0] == 200 || sorted[1] == 200,
        "at least one restore should succeed, got {:?}",
        statuses
    );
}

#[tokio::test]
async fn restore_path_traversal_rejected() {
    let (base, _data, _backup) = spawn_server_persistent_backup().await;
    let client = Client::new();

    for payload in ["../../../etc/passwd", "..%2Fetc", "foo/../../bar"] {
        let resp = client
            .post(format!("{base}/admin/default/restore"))
            .json(&json!({ "backup": payload }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            400,
            "path traversal not blocked for: {payload}"
        );
    }
}

#[tokio::test]
async fn backup_filenames_unique_rapid() {
    let (base, _data, _backup) = spawn_server_persistent_backup().await;
    let client = Client::new();

    let r1: Value = client
        .post(format!("{base}/admin/default/backup"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let r2: Value = client
        .post(format!("{base}/admin/default/backup"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_ne!(
        r1["filename"], r2["filename"],
        "rapid backups should have unique filenames"
    );
}

#[tokio::test]
async fn backup_multi_database_isolation() {
    let (base, _data, _backup) = spawn_server_persistent_backup().await;
    let client = Client::new();

    // Create second database
    client
        .post(format!("{base}/db"))
        .json(&json!({
            "name": "second",
            "storage_mode": "Persistent",
        }))
        .send()
        .await
        .unwrap();

    // Backup both
    client
        .post(format!("{base}/admin/default/backup"))
        .send()
        .await
        .unwrap();
    client
        .post(format!("{base}/admin/second/backup"))
        .send()
        .await
        .unwrap();

    // List-all should have both
    let all: Vec<Value> = client
        .get(format!("{base}/backups"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(all.len(), 2);

    let dbs: std::collections::HashSet<&str> = all
        .iter()
        .map(|b| b["database"].as_str().unwrap())
        .collect();
    assert!(dbs.contains("default"));
    assert!(dbs.contains("second"));

    // Per-database listing should be isolated
    let default_backups: Vec<Value> = client
        .get(format!("{base}/admin/default/backups"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(default_backups.len(), 1);
    assert_eq!(default_backups[0]["database"], "default");

    let second_backups: Vec<Value> = client
        .get(format!("{base}/admin/second/backups"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(second_backups.len(), 1);
    assert_eq!(second_backups[0]["database"], "second");
}

// ---------------------------------------------------------------------------
// GWP Identity-Aware Authentication
// ---------------------------------------------------------------------------

#[cfg(all(feature = "gwp", feature = "auth"))]
async fn spawn_gwp_with_auth(token: &str) -> String {
    let service = grafeo_service::ServiceState::new_in_memory_with_auth(300, token.to_string());
    let gwp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gwp_addr: SocketAddr = gwp_listener.local_addr().unwrap();
    drop(gwp_listener);

    let auth_provider = service.auth().cloned();
    let backend = grafeo_gwp::GrafeoBackend::new(service);
    let options = grafeo_gwp::GwpOptions {
        auth_provider,
        ..Default::default()
    };
    tokio::spawn(async move {
        grafeo_gwp::serve(backend, gwp_addr, options).await.unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    format!("http://{gwp_addr}")
}

#[cfg(all(feature = "gwp", feature = "auth"))]
#[tokio::test]
async fn gwp_auth_bearer_token_allows_query() {
    let gwp_endpoint = spawn_gwp_with_auth("test-gwp-token").await;

    let channel = tonic::transport::Channel::from_shared(gwp_endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut session_client =
        gwp::proto::session_service_client::SessionServiceClient::new(channel.clone());
    let mut gql_client = gwp::proto::gql_service_client::GqlServiceClient::new(channel);

    // Handshake with valid bearer token
    let resp = session_client
        .handshake(gwp::proto::HandshakeRequest {
            protocol_version: 1,
            credentials: Some(gwp::proto::AuthCredentials {
                method: Some(gwp::proto::auth_credentials::Method::BearerToken(
                    "test-gwp-token".to_string(),
                )),
            }),
            client_info: std::collections::HashMap::new(),
        })
        .await
        .expect("handshake with valid token should succeed");

    let session_id = resp.into_inner().session_id;
    assert!(!session_id.is_empty());

    // Execute a query (admin token can write)
    let stream = gql_client
        .execute(gwp::proto::ExecuteRequest {
            session_id: session_id.clone(),
            statement: "CREATE (:GwpAuthTest {val: 42})".to_string(),
            parameters: std::collections::HashMap::new(),
            transaction_id: None,
        })
        .await
        .expect("gRPC call should succeed, error is in stream")
        .into_inner();

    use futures_util::StreamExt as _;
    let mut stream = stream;
    let mut found_success = false;
    while let Some(msg) = stream.next().await {
        if let Ok(resp) = msg {
            if let Some(gwp::proto::execute_response::Frame::Summary(summary)) = resp.frame {
                if let Some(status) = summary.status {
                    if status.code == "00000" {
                        found_success = true;
                    }
                }
            }
        }
    }
    assert!(
        found_success,
        "admin token should allow writes and return success status"
    );

    // Close session
    session_client
        .close_session(gwp::proto::CloseSessionRequest {
            session_id: session_id.clone(),
        })
        .await
        .unwrap();
}

#[cfg(all(feature = "gwp", feature = "auth"))]
#[tokio::test]
async fn gwp_auth_invalid_token_rejected() {
    let gwp_endpoint = spawn_gwp_with_auth("test-gwp-token").await;

    let channel = tonic::transport::Channel::from_shared(gwp_endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut session_client = gwp::proto::session_service_client::SessionServiceClient::new(channel);

    // Handshake with invalid bearer token
    let result = session_client
        .handshake(gwp::proto::HandshakeRequest {
            protocol_version: 1,
            credentials: Some(gwp::proto::AuthCredentials {
                method: Some(gwp::proto::auth_credentials::Method::BearerToken(
                    "wrong-token".to_string(),
                )),
            }),
            client_info: std::collections::HashMap::new(),
        })
        .await;

    assert!(result.is_err(), "invalid token should be rejected");
}

#[cfg(all(feature = "gwp", feature = "auth"))]
#[tokio::test]
async fn gwp_auth_read_only_token_cannot_write() {
    use grafeo_service::auth::{TokenRecord, TokenScope};
    use grafeo_service::token_service::hash_token;
    use grafeo_service::token_store::TokenStore;

    let tmp_dir = tempfile::tempdir().unwrap();
    let store_path = tmp_dir.path().join("tokens.json");
    let store = std::sync::Arc::new(TokenStore::load(&store_path).unwrap());

    // Insert a read-only token
    let ro_token = "read-only-gwp-token";
    store
        .insert(TokenRecord {
            id: "tok-ro".to_string(),
            name: "ro-client".to_string(),
            token_hash: hash_token(ro_token),
            scope: TokenScope {
                role: grafeo_service::auth::Role::ReadOnly,
                databases: vec![],
            },
            created_at: "2026-01-01T00:00:00Z".to_string(),
        })
        .unwrap();

    // Create service with token store
    let provider = grafeo_service::auth::AuthProvider::with_token_store(
        Some("admin-token".to_string()),
        None,
        None,
        store,
    )
    .unwrap();

    let service = grafeo_service::ServiceState::new_in_memory_with_auth_provider(300, provider);

    let gwp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gwp_addr: SocketAddr = gwp_listener.local_addr().unwrap();
    drop(gwp_listener);

    let auth_provider = service.auth().cloned();
    let backend = grafeo_gwp::GrafeoBackend::new(service);
    let options = grafeo_gwp::GwpOptions {
        auth_provider,
        ..Default::default()
    };
    tokio::spawn(async move {
        grafeo_gwp::serve(backend, gwp_addr, options).await.unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let gwp_endpoint = format!("http://{gwp_addr}");
    let channel = tonic::transport::Channel::from_shared(gwp_endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut session_client =
        gwp::proto::session_service_client::SessionServiceClient::new(channel.clone());
    let mut gql_client = gwp::proto::gql_service_client::GqlServiceClient::new(channel);

    // Handshake with read-only token
    let resp = session_client
        .handshake(gwp::proto::HandshakeRequest {
            protocol_version: 1,
            credentials: Some(gwp::proto::AuthCredentials {
                method: Some(gwp::proto::auth_credentials::Method::BearerToken(
                    ro_token.to_string(),
                )),
            }),
            client_info: std::collections::HashMap::new(),
        })
        .await
        .expect("handshake with read-only token should succeed");

    let session_id = resp.into_inner().session_id;

    // Write should fail (read-only identity).
    // GWP wraps errors in the summary frame, so we consume the stream.
    let mut stream = gql_client
        .execute(gwp::proto::ExecuteRequest {
            session_id: session_id.clone(),
            statement: "CREATE (:Forbidden {val: 1})".to_string(),
            parameters: std::collections::HashMap::new(),
            transaction_id: None,
        })
        .await
        .expect("gRPC call should succeed, error is in stream")
        .into_inner();

    use futures_util::StreamExt;
    let mut found_error = false;
    while let Some(msg) = stream.next().await {
        if let Ok(resp) = msg {
            if let Some(gwp::proto::execute_response::Frame::Summary(summary)) = resp.frame {
                if let Some(status) = summary.status {
                    // A non-success GQLSTATUS indicates an error.
                    if status.code != "00000" {
                        found_error = true;
                    }
                }
            }
        }
    }
    assert!(found_error, "read-only token should not allow writes");
}

// ---------------------------------------------------------------------------
// BoltR Identity-Aware Authentication
// ---------------------------------------------------------------------------

#[cfg(all(feature = "bolt", feature = "auth"))]
async fn spawn_bolt_with_auth(token: &str) -> SocketAddr {
    let service = grafeo_service::ServiceState::new_in_memory_with_auth(300, token.to_string());
    let bolt_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bolt_addr: SocketAddr = bolt_listener.local_addr().unwrap();
    drop(bolt_listener);

    let auth_provider = service.auth().cloned();
    let backend = grafeo_boltr::GrafeoBackend::new(service).with_advertise_addr(bolt_addr);
    let options = grafeo_boltr::BoltrOptions {
        auth_provider,
        ..Default::default()
    };
    tokio::spawn(async move {
        grafeo_boltr::serve(backend, bolt_addr, options)
            .await
            .unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    bolt_addr
}

#[cfg(all(feature = "bolt", feature = "auth"))]
#[tokio::test]
async fn bolt_auth_bearer_token_allows_query() {
    let bolt_addr = spawn_bolt_with_auth("test-bolt-token").await;

    // Connect with bearer authentication
    let mut conn = boltr::client::BoltConnection::connect(bolt_addr)
        .await
        .unwrap();
    let extra = boltr::types::BoltDict::from([(
        "user_agent".to_string(),
        boltr::types::BoltValue::String("test-client".to_string()),
    )]);
    conn.hello(extra).await.unwrap();
    conn.logon("bearer", None, Some("test-bolt-token"))
        .await
        .expect("bearer logon with valid token should succeed");

    // Execute a write query (admin token)
    let _meta = conn
        .run(
            "CREATE (:BoltAuthTest {val: 42})",
            std::collections::HashMap::new(),
            boltr::types::BoltDict::new(),
        )
        .await
        .unwrap();
    let _summary = conn.pull_all().await.unwrap();
}

#[cfg(all(feature = "bolt", feature = "auth"))]
#[tokio::test]
async fn bolt_auth_invalid_token_rejected() {
    let bolt_addr = spawn_bolt_with_auth("test-bolt-token").await;

    let mut conn = boltr::client::BoltConnection::connect(bolt_addr)
        .await
        .unwrap();
    let extra = boltr::types::BoltDict::from([(
        "user_agent".to_string(),
        boltr::types::BoltValue::String("test-client".to_string()),
    )]);
    conn.hello(extra).await.unwrap();
    let result = conn.logon("bearer", None, Some("wrong-token")).await;
    assert!(result.is_err(), "invalid token should be rejected");
}

#[cfg(all(feature = "bolt", feature = "auth"))]
#[tokio::test]
async fn bolt_auth_read_only_token_cannot_write() {
    use grafeo_service::auth::{TokenRecord, TokenScope};
    use grafeo_service::token_service::hash_token;
    use grafeo_service::token_store::TokenStore;

    let tmp_dir = tempfile::tempdir().unwrap();
    let store_path = tmp_dir.path().join("tokens.json");
    let store = std::sync::Arc::new(TokenStore::load(&store_path).unwrap());

    // Insert a read-only token
    let ro_token = "read-only-bolt-token";
    store
        .insert(TokenRecord {
            id: "tok-ro".to_string(),
            name: "ro-client".to_string(),
            token_hash: hash_token(ro_token),
            scope: TokenScope {
                role: grafeo_service::auth::Role::ReadOnly,
                databases: vec![],
            },
            created_at: "2026-01-01T00:00:00Z".to_string(),
        })
        .unwrap();

    // Create service with token store
    let provider = grafeo_service::auth::AuthProvider::with_token_store(
        Some("admin-token".to_string()),
        None,
        None,
        store,
    )
    .unwrap();

    let service = grafeo_service::ServiceState::new_in_memory_with_auth_provider(300, provider);

    let bolt_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bolt_addr: SocketAddr = bolt_listener.local_addr().unwrap();
    drop(bolt_listener);

    let auth_provider = service.auth().cloned();
    let backend = grafeo_boltr::GrafeoBackend::new(service).with_advertise_addr(bolt_addr);
    let options = grafeo_boltr::BoltrOptions {
        auth_provider,
        ..Default::default()
    };
    tokio::spawn(async move {
        grafeo_boltr::serve(backend, bolt_addr, options)
            .await
            .unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Connect with read-only bearer token
    let mut conn = boltr::client::BoltConnection::connect(bolt_addr)
        .await
        .unwrap();
    let extra = boltr::types::BoltDict::from([(
        "user_agent".to_string(),
        boltr::types::BoltValue::String("test-client".to_string()),
    )]);
    conn.hello(extra).await.unwrap();
    conn.logon("bearer", None, Some(ro_token))
        .await
        .expect("logon with read-only token should succeed");

    // Write should fail (read-only identity)
    let result = conn
        .run(
            "CREATE (:Forbidden {val: 1})",
            std::collections::HashMap::new(),
            boltr::types::BoltDict::new(),
        )
        .await;
    assert!(result.is_err(), "read-only token should not allow writes");
}

// ---------------------------------------------------------------------------
// HTTP Auth: Scoped Tokens (database filtering, access checks, identity)
// ---------------------------------------------------------------------------

/// Helper: creates a server with a token store containing a legacy admin token
/// and one or more managed scoped tokens. Returns (base_url, admin_token, Vec<(token_plaintext, token_id)>).
#[cfg(feature = "auth")]
async fn spawn_server_with_token_store(
    admin_token: &str,
    scoped_tokens: Vec<(&str, &str, grafeo_service::auth::TokenScope)>,
) -> (String, String, Vec<(String, String)>) {
    use grafeo_service::auth::TokenRecord;

    let dir = tempfile::tempdir().unwrap();
    let store =
        grafeo_service::token_store::TokenStore::load(dir.path().join("tokens.json")).unwrap();

    let mut token_infos = Vec::new();
    for (plaintext, name, scope) in &scoped_tokens {
        let hash = grafeo_service::token_service::hash_token(plaintext);
        let id = format!("tok-{name}");
        store
            .insert(TokenRecord {
                id: id.clone(),
                name: name.to_string(),
                token_hash: hash,
                scope: scope.clone(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
            })
            .unwrap();
        token_infos.push((plaintext.to_string(), id));
    }

    let store_arc = std::sync::Arc::new(store);
    let provider = grafeo_service::auth::AuthProvider::with_token_store(
        Some(admin_token.to_string()),
        None,
        None,
        store_arc,
    )
    .unwrap();

    let service = grafeo_service::ServiceState::new_in_memory_with_auth_provider(300, provider);
    let state = grafeo_server::AppState::new(
        service,
        vec![],
        grafeo_service::types::EnabledFeatures::default(),
    );
    let base = spawn_server_from_state(state).await;
    (base, admin_token.to_string(), token_infos)
}

/// Database list filtering: a scoped token sees only its databases.
#[cfg(feature = "auth")]
#[tokio::test]
async fn auth_scoped_token_filters_database_list() {
    use grafeo_service::auth::TokenScope;

    let scope = TokenScope {
        role: grafeo_service::auth::Role::ReadWrite,
        databases: vec!["db1".to_string()],
    };
    let (base, admin_token, tokens) =
        spawn_server_with_token_store("admin-tok", vec![("scoped-tok", "scoped-svc", scope)]).await;
    let scoped_token = &tokens[0].0;
    let client = Client::new();

    // Create db1 and db2 with admin token
    let resp = client
        .post(format!("{base}/db"))
        .header("Authorization", format!("Bearer {admin_token}"))
        .json(&json!({"name": "db1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = client
        .post(format!("{base}/db"))
        .header("Authorization", format!("Bearer {admin_token}"))
        .json(&json!({"name": "db2"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Admin sees all three databases (default + db1 + db2)
    let resp = client
        .get(format!("{base}/db"))
        .header("Authorization", format!("Bearer {admin_token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let dbs = body["databases"].as_array().unwrap();
    assert!(dbs.len() >= 3, "admin should see all databases");

    // Scoped token sees only db1
    let resp = client
        .get(format!("{base}/db"))
        .header("Authorization", format!("Bearer {scoped_token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let dbs = body["databases"].as_array().unwrap();
    assert_eq!(dbs.len(), 1, "scoped token should see only db1");
    assert_eq!(dbs[0]["name"], "db1");
}

/// Database access check: scoped token cannot create a database outside its scope.
#[cfg(feature = "auth")]
#[tokio::test]
async fn auth_scoped_token_cannot_create_db_outside_scope() {
    use grafeo_service::auth::TokenScope;

    let scope = TokenScope {
        role: grafeo_service::auth::Role::ReadWrite,
        databases: vec!["allowed-db".to_string()],
    };
    let (base, _admin_token, tokens) = spawn_server_with_token_store(
        "admin-tok-2",
        vec![("scoped-tok-2", "scoped-create", scope)],
    )
    .await;
    let scoped_token = &tokens[0].0;
    let client = Client::new();

    // Scoped token tries to create a database outside its scope: should be 403
    let resp = client
        .post(format!("{base}/db"))
        .header("Authorization", format!("Bearer {scoped_token}"))
        .json(&json!({"name": "forbidden-db"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        403,
        "scoped token should not create databases outside its scope"
    );

    // Scoped token can create a database within its scope
    let resp = client
        .post(format!("{base}/db"))
        .header("Authorization", format!("Bearer {scoped_token}"))
        .json(&json!({"name": "allowed-db"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "scoped token should be able to create databases within its scope"
    );
}

/// Database access check: scoped token cannot delete a database outside its scope.
#[cfg(feature = "auth")]
#[tokio::test]
async fn auth_scoped_token_cannot_delete_db_outside_scope() {
    use grafeo_service::auth::TokenScope;

    let scope = TokenScope {
        role: grafeo_service::auth::Role::ReadWrite,
        databases: vec!["my-db".to_string()],
    };
    let (base, admin_token, tokens) =
        spawn_server_with_token_store("admin-tok-3", vec![("scoped-tok-3", "scoped-del", scope)])
            .await;
    let scoped_token = &tokens[0].0;
    let client = Client::new();

    // Admin creates a database the scoped token does not have access to
    let resp = client
        .post(format!("{base}/db"))
        .header("Authorization", format!("Bearer {admin_token}"))
        .json(&json!({"name": "other-db"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Scoped token tries to delete it: 403
    let resp = client
        .delete(format!("{base}/db/other-db"))
        .header("Authorization", format!("Bearer {scoped_token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        403,
        "scoped token should not delete databases outside its scope"
    );
}

/// Transaction owner mismatch: begin with one token, query with another.
#[cfg(feature = "auth")]
#[tokio::test]
async fn auth_tx_owner_mismatch_rejected() {
    use grafeo_service::auth::TokenScope;

    let scope_a = TokenScope {
        role: grafeo_service::auth::Role::ReadWrite,
        databases: vec![],
    };
    let scope_b = TokenScope {
        role: grafeo_service::auth::Role::ReadWrite,
        databases: vec![],
    };
    let (base, _admin_token, tokens) = spawn_server_with_token_store(
        "admin-tok-4",
        vec![("token-a", "svc-a", scope_a), ("token-b", "svc-b", scope_b)],
    )
    .await;
    let token_a = &tokens[0].0;
    let token_b = &tokens[1].0;
    let client = Client::new();

    // Token A begins a transaction
    let resp = client
        .post(format!("{base}/tx/begin"))
        .header("Authorization", format!("Bearer {token_a}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let session_id = body["session_id"].as_str().unwrap().to_string();

    // Token A can query the transaction
    let resp = client
        .post(format!("{base}/tx/query"))
        .header("Authorization", format!("Bearer {token_a}"))
        .header("X-Session-Id", &session_id)
        .json(&json!({"query": "MATCH (n) RETURN count(n)"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "owner token should be able to query");

    // Token B tries to query the same transaction: should fail (404, session not found)
    let resp = client
        .post(format!("{base}/tx/query"))
        .header("Authorization", format!("Bearer {token_b}"))
        .header("X-Session-Id", &session_id)
        .json(&json!({"query": "MATCH (n) RETURN count(n)"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "different token should not be able to query another token's transaction"
    );

    // Token B tries to commit: should also fail
    let resp = client
        .post(format!("{base}/tx/commit"))
        .header("Authorization", format!("Bearer {token_b}"))
        .header("X-Session-Id", &session_id)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "different token should not be able to commit another token's transaction"
    );

    // Token A can still commit successfully
    let resp = client
        .post(format!("{base}/tx/commit"))
        .header("Authorization", format!("Bearer {token_a}"))
        .header("X-Session-Id", &session_id)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "owner token should be able to commit its own transaction"
    );
}

/// Graph store with auth identity: PUT and GET on /db/{name}/graph-store with a scoped token.
#[cfg(feature = "auth")]
#[tokio::test]
async fn auth_graph_store_with_scoped_token() {
    use grafeo_service::auth::TokenScope;

    let scope = TokenScope {
        role: grafeo_service::auth::Role::ReadWrite,
        databases: vec!["default".to_string()],
    };
    let (base, admin_token, tokens) = spawn_server_with_token_store(
        "admin-tok-5",
        vec![("scoped-gs-tok", "graph-store-svc", scope)],
    )
    .await;
    let scoped_token = &tokens[0].0;
    let client = Client::new();

    // Scoped token can PUT to graph-store on "default" (its scope)
    let resp = client
        .put(format!("{base}/db/default/graph-store?default"))
        .header("Authorization", format!("Bearer {scoped_token}"))
        .header("content-type", "application/n-triples")
        .body("")
        .send()
        .await
        .unwrap();
    // SPARQL might not be enabled: accept 204 (success) or 400 (sparql disabled)
    assert!(
        resp.status() == 204 || resp.status() == 400,
        "scoped token should reach graph-store handler, got {}",
        resp.status()
    );

    // Scoped token can GET from graph-store on "default"
    let resp = client
        .get(format!("{base}/db/default/graph-store?default"))
        .header("Authorization", format!("Bearer {scoped_token}"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status() == 200 || resp.status() == 400,
        "scoped token should reach graph-store GET, got {}",
        resp.status()
    );

    // Admin creates "other-db"
    let resp = client
        .post(format!("{base}/db"))
        .header("Authorization", format!("Bearer {admin_token}"))
        .json(&json!({"name": "otherdb"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Admin can access graph-store on "otherdb"
    let resp = client
        .get(format!("{base}/db/otherdb/graph-store?default"))
        .header("Authorization", format!("Bearer {admin_token}"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status() == 200 || resp.status() == 400,
        "admin should reach graph-store on otherdb, got {}",
        resp.status()
    );
}

/// Transaction rollback with owner mismatch.
#[cfg(feature = "auth")]
#[tokio::test]
async fn auth_tx_rollback_owner_mismatch_rejected() {
    use grafeo_service::auth::TokenScope;

    let scope_a = TokenScope {
        role: grafeo_service::auth::Role::ReadWrite,
        databases: vec![],
    };
    let scope_b = TokenScope {
        role: grafeo_service::auth::Role::ReadWrite,
        databases: vec![],
    };
    let (base, _admin_token, tokens) = spawn_server_with_token_store(
        "admin-tok-6",
        vec![
            ("tok-owner", "owner-svc", scope_a),
            ("tok-intruder", "intruder-svc", scope_b),
        ],
    )
    .await;
    let owner_token = &tokens[0].0;
    let intruder_token = &tokens[1].0;
    let client = Client::new();

    // Owner begins a transaction
    let resp = client
        .post(format!("{base}/tx/begin"))
        .header("Authorization", format!("Bearer {owner_token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let session_id = body["session_id"].as_str().unwrap().to_string();

    // Intruder tries to rollback: should fail
    let resp = client
        .post(format!("{base}/tx/rollback"))
        .header("Authorization", format!("Bearer {intruder_token}"))
        .header("X-Session-Id", &session_id)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "different token should not rollback another token's transaction"
    );

    // Owner can rollback
    let resp = client
        .post(format!("{base}/tx/rollback"))
        .header("Authorization", format!("Bearer {owner_token}"))
        .header("X-Session-Id", &session_id)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "owner token should be able to rollback its own transaction"
    );
}
