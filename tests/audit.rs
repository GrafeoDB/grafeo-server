#![cfg(all(feature = "http", feature = "auth"))]
//! Audit log (#84): failed authentication is recorded with the source IP,
//! token and database lifecycle events name the actor, and write statements
//! are recorded only with `--audit-data-changes`. Events are JSON lines in
//! the audit log file; token plaintexts are never logged.

use std::net::SocketAddr;
use std::path::PathBuf;

use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tokio::net::TcpListener;

const ROOT: &str = "root-token";

/// Boots a server with a token store (bootstrap token `ROOT`) and the audit
/// log written to a file. Returns the base URL, the audit log path and the
/// tempdir keeping both alive.
async fn spawn_audit(data_changes: bool) -> (String, PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let audit_path = dir.path().join("audit.log");
    let store =
        grafeo_service::token_store::TokenStore::load(dir.path().join("tokens.json")).unwrap();
    let provider = grafeo_service::auth::AuthProvider::with_token_store(
        Some(ROOT.to_string()),
        None,
        None,
        std::sync::Arc::new(store),
    )
    .unwrap();
    let service = grafeo_service::ServiceState::new_in_memory_with_auth_provider(300, provider)
        .with_audit(grafeo_service::audit::AuditConfig {
            log_file: Some(audit_path.clone()),
            data_changes,
            ..Default::default()
        })
        .unwrap();
    let state = grafeo_server::AppState::new(
        service,
        vec![],
        grafeo_service::types::EnabledFeatures::default(),
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
    (format!("http://{addr}"), audit_path, dir)
}

/// Reads the audit log file as parsed JSON lines.
fn audit_lines(path: &std::path::Path) -> Vec<Value> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    content
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn bearer(client: &Client, base: &str, path: &str, token: &str) -> reqwest::RequestBuilder {
    client
        .get(format!("{base}{path}"))
        .header("Authorization", format!("Bearer {token}"))
}

#[tokio::test]
async fn auth_failure_is_logged_with_source_ip() {
    let (base, audit_path, _dir) = spawn_audit(false).await;
    let client = Client::new();

    let resp = bearer(&client, &base, "/db", "wrong-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let lines = audit_lines(&audit_path);
    let event = lines
        .iter()
        .find(|l| l["event"] == "auth_failure")
        .expect("an auth_failure event");
    assert_eq!(event["transport"], "http");
    assert_eq!(event["source_ip"], "127.0.0.1");
    assert!(event["detail"].as_str().unwrap().contains("GET /db"));
    assert!(event["ts"].as_str().unwrap().ends_with('Z'));
}

#[tokio::test]
async fn access_denied_is_logged_with_actor() {
    let (base, audit_path, _dir) = spawn_audit(false).await;
    let client = Client::new();

    // A read-only token cannot create databases (#82): 403, audited.
    let store_resp = client
        .post(format!("{base}/admin/tokens"))
        .header("Authorization", format!("Bearer {ROOT}"))
        .json(&json!({"name": "ro", "scope": {"role": "read-only"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), StatusCode::OK);
    let ro_token = store_resp.json::<Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = client
        .post(format!("{base}/db"))
        .header("Authorization", format!("Bearer {ro_token}"))
        .json(&json!({"name": "nope"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let lines = audit_lines(&audit_path);
    let event = lines
        .iter()
        .find(|l| l["event"] == "access_denied")
        .expect("an access_denied event");
    assert_eq!(event["actor"], "ro");
    assert_eq!(event["transport"], "http");
    assert_eq!(event["source_ip"], "127.0.0.1");
}

#[tokio::test]
async fn token_lifecycle_is_logged_without_the_plaintext() {
    let (base, audit_path, _dir) = spawn_audit(false).await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/admin/tokens"))
        .header("Authorization", format!("Bearer {ROOT}"))
        .json(&json!({"name": "ci", "scope": {"role": "read-write"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.json::<Value>().await.unwrap();
    let id = body["id"].as_str().unwrap().to_string();
    let plaintext = body["token"].as_str().unwrap().to_string();

    let resp = client
        .delete(format!("{base}/admin/tokens/{id}"))
        .header("Authorization", format!("Bearer {ROOT}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let lines = audit_lines(&audit_path);
    let created = lines
        .iter()
        .find(|l| l["event"] == "token_created")
        .expect("a token_created event");
    assert!(created["detail"].as_str().unwrap().contains("ci"));
    let revoked = lines
        .iter()
        .find(|l| l["event"] == "token_revoked")
        .expect("a token_revoked event");
    assert!(revoked["detail"].as_str().unwrap().contains(&id));

    let raw = std::fs::read_to_string(&audit_path).unwrap();
    assert!(
        !raw.contains(&plaintext),
        "the plaintext token is never logged"
    );
}

#[tokio::test]
async fn database_lifecycle_is_logged() {
    let (base, audit_path, _dir) = spawn_audit(false).await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/db"))
        .header("Authorization", format!("Bearer {ROOT}"))
        .json(&json!({"name": "auditdb"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = client
        .delete(format!("{base}/db/auditdb"))
        .header("Authorization", format!("Bearer {ROOT}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let lines = audit_lines(&audit_path);
    let created = lines
        .iter()
        .find(|l| l["event"] == "database_created")
        .expect("a database_created event");
    assert_eq!(created["database"], "auditdb");
    assert!(created["actor"].is_string());
    assert!(
        lines
            .iter()
            .any(|l| l["event"] == "database_deleted" && l["database"] == "auditdb")
    );
}

#[tokio::test]
async fn data_changes_are_logged_only_when_enabled() {
    let (base, audit_path, _dir) = spawn_audit(true).await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/query"))
        .header("Authorization", format!("Bearer {ROOT}"))
        .json(&json!({"query": "INSERT (:Person {name: 'Audit'})"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let lines = audit_lines(&audit_path);
    let event = lines
        .iter()
        .find(|l| l["event"] == "data_change")
        .expect("a data_change event");
    assert_eq!(event["database"], "default");
    assert_eq!(event["transport"], "http");
    assert!(event["detail"].as_str().unwrap().contains("INSERT"));

    // With data changes off, a write is not audited.
    let (base2, audit_path2, _dir2) = spawn_audit(false).await;
    let resp = client
        .post(format!("{base2}/query"))
        .header("Authorization", format!("Bearer {ROOT}"))
        .json(&json!({"query": "INSERT (:Person {name: 'Quiet'})"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        !audit_lines(&audit_path2)
            .iter()
            .any(|l| l["event"] == "data_change")
    );
}
