#![cfg(feature = "http")]
//! Integration tests for the Grafeo Server HTTP API.
//!
//! Each test starts an in-memory server on an ephemeral port and uses reqwest
//! to exercise the endpoints.

use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde_json::{Value, json};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite;

/// Boots an in-memory Grafeo server on an OS-assigned port.
/// Returns the base URL (e.g. "http://127.0.0.1:12345").
async fn spawn_server() -> String {
    // Inline the same setup as main.rs but with in-memory config
    let state = grafeo_server::AppState::new_in_memory(300);
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
    let mut db_client = conn.create_database_client();

    let databases = db_client.list().await.unwrap();
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
    let mut db_client = conn.create_database_client();

    // Create a database
    let info = db_client
        .create(gwp::server::CreateDatabaseConfig {
            name: "gwp-test-db".to_string(),
            database_type: "lpg".to_string(),
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
    let databases = db_client.list().await.unwrap();
    assert_eq!(databases.len(), 2);

    // Delete
    let deleted = db_client.delete("gwp-test-db").await.unwrap();
    assert_eq!(deleted, "gwp-test-db");

    // List should show 1 database
    let databases = db_client.list().await.unwrap();
    assert_eq!(databases.len(), 1);
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_get_database_info() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let conn = gwp::client::GqlConnection::connect(&gwp_endpoint)
        .await
        .unwrap();
    let mut db_client = conn.create_database_client();

    let info = db_client.get_info("default").await.unwrap();
    assert_eq!(info.name, "default");
    assert_eq!(info.database_type, "lpg");
    assert!(!info.persistent);
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_create_database_then_query() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let conn = gwp::client::GqlConnection::connect(&gwp_endpoint)
        .await
        .unwrap();

    // Create database via GWP
    let mut db_client = conn.create_database_client();
    db_client
        .create(gwp::server::CreateDatabaseConfig {
            name: "query-db".to_string(),
            database_type: "lpg".to_string(),
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
    let info = db_client.get_info("query-db").await.unwrap();
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
    let mut db_client = conn.create_database_client();

    let config = gwp::server::CreateDatabaseConfig {
        name: "ephemeral".to_string(),
        database_type: "lpg".to_string(),
        storage_mode: "inmemory".to_string(),
        memory_limit_bytes: None,
        backward_edges: None,
        threads: None,
        wal_enabled: None,
        wal_durability: None,
    };

    // Create, delete, recreate — exercises the close barrier path
    db_client.create(config.clone()).await.unwrap();
    db_client.delete("ephemeral").await.unwrap();
    db_client.create(config).await.unwrap();

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
    let mut db_client = conn.create_database_client();

    let result = db_client.delete("nonexistent").await;
    assert!(result.is_err(), "deleting nonexistent database should fail");
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_create_duplicate_database_fails() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let conn = gwp::client::GqlConnection::connect(&gwp_endpoint)
        .await
        .unwrap();
    let mut db_client = conn.create_database_client();

    let config = gwp::server::CreateDatabaseConfig {
        name: "dup".to_string(),
        database_type: "lpg".to_string(),
        storage_mode: "inmemory".to_string(),
        memory_limit_bytes: None,
        backward_edges: None,
        threads: None,
        wal_enabled: None,
        wal_durability: None,
    };

    db_client.create(config.clone()).await.unwrap();
    let result = db_client.create(config).await;
    assert!(result.is_err(), "creating duplicate database should fail");
}

#[cfg(feature = "gwp")]
#[tokio::test]
async fn gwp_configure_deleted_database_fails() {
    let (_http, gwp_endpoint) = spawn_server_with_gwp().await;

    let conn = gwp::client::GqlConnection::connect(&gwp_endpoint)
        .await
        .unwrap();
    let mut db_client = conn.create_database_client();

    // Create then delete a database
    db_client
        .create(gwp::server::CreateDatabaseConfig {
            name: "doomed".to_string(),
            database_type: "lpg".to_string(),
            storage_mode: "inmemory".to_string(),
            memory_limit_bytes: None,
            backward_edges: None,
            threads: None,
            wal_enabled: None,
            wal_durability: None,
        })
        .await
        .unwrap();
    db_client.delete("doomed").await.unwrap();

    // Configuring a session to the deleted database should fail
    let mut session = conn.create_session().await.unwrap();
    let result = session.set_graph("doomed").await;
    assert!(
        result.is_err(),
        "configuring session to deleted database should fail"
    );

    session.close().await.unwrap();
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
        .get_database_stats(gwp::proto::GetDatabaseStatsRequest {
            database: "default".to_string(),
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
            database: "default".to_string(),
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
            database: "default".to_string(),
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
            database: "default".to_string(),
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
            database: "default".to_string(),
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
    let backend = grafeo_boltr::GrafeoBackend::new(state.service().clone());
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
    session.run("CREATE (:BoltTxTest {val: 42})").await.unwrap();
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
    session.run("CREATE (:BoltRbTest {val: 99})").await.unwrap();
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
    let result = session
        .run("MATCH (n) RETURN n LIMIT 1")
        .await
        .unwrap();
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

    session
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
    session
        .run("CREATE (:BoltMulti {seq: 1})")
        .await
        .unwrap();
    session
        .run("CREATE (:BoltMulti {seq: 2})")
        .await
        .unwrap();
    session
        .run("CREATE (:BoltMulti {seq: 3})")
        .await
        .unwrap();

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
