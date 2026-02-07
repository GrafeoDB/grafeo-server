//! Integration tests for the Grafeo Server HTTP API.
//!
//! Each test starts an in-memory server on an ephemeral port and uses reqwest
//! to exercise the endpoints.

use reqwest::Client;
use serde_json::{Value, json};
use std::net::SocketAddr;
use tokio::net::TcpListener;

/// Boots an in-memory Grafeo server on an OS-assigned port.
/// Returns the base URL (e.g. "http://127.0.0.1:12345").
async fn spawn_server() -> String {
    // Inline the same setup as main.rs but with in-memory config
    let state = grafeo_server::AppState::new_in_memory(300);
    let app = grafeo_server::router(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
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
    assert_eq!(body["info"]["version"], "0.2.0");

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
// Authentication
// ---------------------------------------------------------------------------

async fn spawn_server_with_auth(token: &str) -> String {
    let state = grafeo_server::AppState::new_in_memory_with_auth(300, token.to_string());
    let app = grafeo_server::router(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    format!("http://{addr}")
}

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

#[tokio::test]
async fn health_exempt_from_auth() {
    let base = spawn_server_with_auth("secret-token").await;
    let client = Client::new();

    let resp = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn metrics_exempt_from_auth() {
    let base = spawn_server_with_auth("secret-token").await;
    let client = Client::new();

    let resp = client.get(format!("{base}/metrics")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
}

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
