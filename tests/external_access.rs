#![cfg(feature = "http")]
//! External access over every transport: by default no remote statement may
//! reach outside the database (`LOAD CSV`, `grafeo.load_nodes_csv`), admin
//! included, and the refusal is a 403 naming external access (GWP and Bolt
//! refuse with their client error). With `--allow-external-access` an admin
//! may, and a read-write or read-only token still may not.

use std::net::SocketAddr;

use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tokio::net::TcpListener;

/// A CSV file of two people on the server's disk, its path as a query writes it.
fn people_csv() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("people.csv");
    std::fs::write(&path, "id,name\n1,Alix\n2,Gus\n").unwrap();
    let text = path.to_string_lossy().replace('\\', "/");
    (dir, text)
}

fn load_csv(path: &str) -> String {
    format!("LOAD CSV WITH HEADERS FROM '{path}' AS row RETURN row.name AS name")
}

fn load_procedure(path: &str) -> String {
    format!("CALL grafeo.load_nodes_csv({{path: '{path}', labels: ['Leak'], key: 'id'}})")
}

async fn serve_http(state: grafeo_server::AppState) -> String {
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

fn app_state(service: grafeo_service::ServiceState) -> grafeo_server::AppState {
    grafeo_server::AppState::new(
        service,
        vec![],
        grafeo_service::types::EnabledFeatures::default(),
    )
}

/// An unauthenticated server (every client is the anonymous admin).
fn open_service(external_access: bool) -> grafeo_service::ServiceState {
    grafeo_service::ServiceState::new_in_memory(300).with_external_access(external_access)
}

/// Refused: a 403 whose detail names external access.
async fn assert_refused(resp: reqwest::Response, what: &str) {
    let status = resp.status();
    let body = resp.text().await.unwrap();
    assert_eq!(status, StatusCode::FORBIDDEN, "{what}: {body}");
    assert!(
        body.contains("external access"),
        "{what}: the refusal names external access: {body}"
    );
    assert!(
        body.contains("--allow-external-access"),
        "{what}: the refusal names the server flag: {body}"
    );
}

/// Every HTTP way a statement runs, for one statement in one language.
async fn http_attempts(
    client: &Client,
    base: &str,
    auth: Option<&str>,
    query: &str,
    language: &str,
) -> Vec<(String, reqwest::Response)> {
    let with_auth = |r: reqwest::RequestBuilder| match auth {
        Some(token) => r.header("Authorization", format!("Bearer {token}")),
        None => r,
    };
    let mut out = Vec::new();

    let resp = with_auth(client.post(format!("{base}/query")))
        .json(&json!({"query": query, "language": language}))
        .send()
        .await
        .unwrap();
    out.push((format!("/query ({language})"), resp));

    let resp = with_auth(client.post(format!("{base}/batch")))
        .json(&json!({"queries": [{"query": query, "language": language}]}))
        .send()
        .await
        .unwrap();
    out.push((format!("/batch ({language})"), resp));

    let begin = with_auth(client.post(format!("{base}/tx/begin")))
        .send()
        .await
        .unwrap();
    assert_eq!(begin.status(), StatusCode::OK);
    let session_id = begin.json::<Value>().await.unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let resp = with_auth(client.post(format!("{base}/tx/query")))
        .header("X-Session-Id", &session_id)
        .json(&json!({"query": query, "language": language}))
        .send()
        .await
        .unwrap();
    out.push((format!("/tx/query ({language})"), resp));
    let _ = with_auth(client.post(format!("{base}/tx/rollback")))
        .header("X-Session-Id", &session_id)
        .send()
        .await;
    out
}

#[tokio::test]
async fn by_default_http_refuses_every_file_read() {
    let (_dir, path) = people_csv();
    let base = serve_http(app_state(open_service(false))).await;
    let client = Client::new();

    for (query, language) in [(load_csv(&path), "cypher"), (load_procedure(&path), "gql")] {
        for (what, resp) in http_attempts(&client, &base, None, &query, language).await {
            assert_refused(resp, &what).await;
        }
    }
    let resp = client
        .post(format!("{base}/cypher"))
        .json(&json!({"query": load_csv(&path)}))
        .send()
        .await
        .unwrap();
    assert_refused(resp, "/cypher").await;

    // A refused load wrote nothing.
    let resp = client
        .post(format!("{base}/query"))
        .json(&json!({"query": "MATCH (n:Leak) RETURN count(n) AS c"}))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["rows"][0][0], 0, "{body}");
}

#[tokio::test]
async fn with_the_flag_the_anonymous_admin_reads_files() {
    let (_dir, path) = people_csv();
    let base = serve_http(app_state(open_service(true))).await;
    let client = Client::new();

    for (what, resp) in http_attempts(&client, &base, None, &load_csv(&path), "cypher").await {
        let status = resp.status();
        let body = resp.text().await.unwrap();
        assert_eq!(status, StatusCode::OK, "{what}: {body}");
        assert!(
            body.contains("Alix") && body.contains("Gus"),
            "{what}: {body}"
        );
    }
}

/// A server with tokens: the bootstrap admin token `ROOT` and a read-write
/// and a read-only token made through the API. Returns the base URL and the
/// admin, read-write and read-only tokens.
#[cfg(feature = "auth")]
async fn token_server(external_access: bool) -> (String, String, String, String) {
    const ROOT: &str = "root-token";
    let dir = tempfile::tempdir().unwrap();
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
        .with_external_access(external_access);
    let base = serve_http(app_state(service)).await;
    std::mem::forget(dir);
    let client = Client::new();
    let mut tokens = Vec::new();
    for (name, role) in [("rw", "read-write"), ("ro", "read-only")] {
        let resp = client
            .post(format!("{base}/admin/tokens"))
            .header("Authorization", format!("Bearer {ROOT}"))
            .json(&json!({"name": name, "scope": {"role": role}}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        tokens.push(
            resp.json::<Value>().await.unwrap()["token"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    let rw = tokens.remove(0);
    let ro = tokens.remove(0);
    (base, ROOT.to_string(), rw, ro)
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn with_the_flag_only_admin_tokens_read_files() {
    let (_dir, path) = people_csv();
    let (base, root, rw, ro) = token_server(true).await;
    let client = Client::new();

    for (what, resp) in http_attempts(&client, &base, Some(&root), &load_csv(&path), "cypher").await
    {
        assert_eq!(resp.status(), StatusCode::OK, "admin {what}");
    }
    for (who, token) in [("read-write", &rw), ("read-only", &ro)] {
        for (what, resp) in
            http_attempts(&client, &base, Some(token), &load_csv(&path), "cypher").await
        {
            assert_refused(resp, &format!("{who} {what}")).await;
        }
    }
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn without_the_flag_even_admin_tokens_are_refused() {
    let (_dir, path) = people_csv();
    let (base, root, _rw, _ro) = token_server(false).await;
    let client = Client::new();
    for (what, resp) in http_attempts(&client, &base, Some(&root), &load_csv(&path), "cypher").await
    {
        assert_refused(resp, &format!("admin {what}")).await;
    }
}

/// A Turtle file of one triple on the server's disk.
fn one_triple_ttl() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("people.ttl");
    std::fs::write(&path, "<urn:alix> <urn:knows> <urn:gus> .\n").unwrap();
    let text = path.to_string_lossy().replace('\\', "/");
    (dir, text)
}

async fn run_query(
    client: &Client,
    base: &str,
    auth: Option<&str>,
    query: &str,
    language: &str,
) -> (StatusCode, String) {
    let mut request = client
        .post(format!("{base}/query"))
        .json(&json!({"query": query, "language": language}));
    if let Some(token) = auth {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let resp = request.send().await.unwrap();
    (resp.status(), resp.text().await.unwrap())
}

/// GQL `LOAD DATA` and SPARQL `LOAD`, the other ways a query reads a file:
/// refused by default with the flag named, not refused for an admin with the
/// flag (SPARQL `LOAD` then fails as not supported yet, not as a refusal).
#[tokio::test]
async fn gql_load_data_and_sparql_load_are_guarded() {
    let (_csv_dir, csv) = people_csv();
    let (_ttl_dir, ttl) = one_triple_ttl();
    let load_data =
        format!("LOAD DATA FROM '{csv}' FORMAT CSV WITH HEADERS AS row RETURN row.name AS name");
    let sparql_load = format!("LOAD <file:///{ttl}>");
    let client = Client::new();

    let closed = serve_http(app_state(open_service(false))).await;
    for (query, language) in [(&load_data, "gql"), (&sparql_load, "sparql")] {
        let (status, body) = run_query(&client, &closed, None, query, language).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{language}: {body}");
        assert!(
            body.contains("--allow-external-access"),
            "{language}: {body}"
        );
    }

    let open = serve_http(app_state(open_service(true))).await;
    let (status, body) = run_query(&client, &open, None, &load_data, "gql").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Alix") && body.contains("Gus"), "{body}");
    let (status, body) = run_query(&client, &open, None, &sparql_load, "sparql").await;
    assert_ne!(
        status,
        StatusCode::FORBIDDEN,
        "not a refusal for the admin: {body}"
    );
    assert!(!body.contains("external access"), "{body}");
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn gql_load_data_and_sparql_load_stay_refused_for_non_admins() {
    let (_csv_dir, csv) = people_csv();
    let (_ttl_dir, ttl) = one_triple_ttl();
    let (base, _root, rw, ro) = token_server(true).await;
    let client = Client::new();
    for token in [&rw, &ro] {
        for (query, language) in [
            (
                format!("LOAD DATA FROM '{csv}' FORMAT CSV WITH HEADERS AS row RETURN row.name"),
                "gql",
            ),
            (format!("LOAD <file:///{ttl}>"), "sparql"),
        ] {
            let (status, body) = run_query(&client, &base, Some(token), &query, language).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{language}: {body}");
            assert!(body.contains("external access"), "{language}: {body}");
        }
    }
}

/// On a read-only server with the flag, an admin may read a file but not
/// write: a `LOAD CSV ... RETURN` answers, a load into the graph is refused
/// as a write (not as external access), and nothing is stored.
#[tokio::test]
async fn on_a_read_only_server_an_admin_reads_files_but_writes_nothing() {
    let (_dir, path) = people_csv();
    let service =
        grafeo_service::ServiceState::new_in_memory_read_only(300).with_external_access(true);
    let base = serve_http(app_state(service)).await;
    let client = Client::new();

    let (status, body) = run_query(&client, &base, None, &load_csv(&path), "cypher").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Alix") && body.contains("Gus"), "{body}");

    let (status, body) = run_query(&client, &base, None, &load_procedure(&path), "gql").await;
    assert!(
        status.is_client_error(),
        "a write on a read-only server is refused: {body}"
    );
    assert!(
        !body.contains("external access"),
        "refused as a write: {body}"
    );

    let (_, body) = run_query(
        &client,
        &base,
        None,
        "MATCH (n:Leak) RETURN count(n) AS c",
        "gql",
    )
    .await;
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["rows"][0][0], 0, "nothing was stored: {body}");
}

#[cfg(feature = "gwp")]
mod over_gwp {
    use super::*;

    async fn serve_gwp(service: grafeo_service::ServiceState) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let backend = grafeo_gwp::GrafeoBackend::new(service);
        tokio::spawn(async move {
            gwp::server::GqlServer::builder(backend)
                .serve_with_listener(listener)
                .await
                .unwrap();
        });
        format!("http://{addr}")
    }

    /// Runs the load procedure over GWP: `Ok` with the GQLSTATUS of success,
    /// `Err((code, message))` with the failure's status (GWP reports a failed
    /// statement in the result summary, not as a transport error).
    async fn gwp_load(endpoint: &str, path: &str) -> Result<(), (String, String)> {
        let conn = gwp::client::GqlConnection::connect(endpoint).await.unwrap();
        let mut session = conn.create_session().await.unwrap();
        let mut cursor = session
            .execute(&load_procedure(path), std::collections::HashMap::new())
            .await
            .unwrap();
        let success = cursor.is_success().await.unwrap();
        let status = cursor
            .summary()
            .await
            .unwrap()
            .and_then(|s| s.status.clone())
            .map_or_default(|s| (s.code, s.message));
        drop(cursor);
        let _ = session.close().await;
        if success { Ok(()) } else { Err(status) }
    }

    #[tokio::test]
    async fn gwp_refuses_by_default_and_admits_the_admin_with_the_flag() {
        let (_dir, path) = people_csv();
        let refused = gwp_load(&serve_gwp(open_service(false)).await, &path).await;
        let (code, message) = refused.expect_err("refused by default");
        assert_eq!(code, "42501", "a permission refusal: {message}");
        assert!(message.contains("external access"), "{message}");
        assert!(message.contains("--allow-external-access"), "{message}");

        let allowed = gwp_load(&serve_gwp(open_service(true)).await, &path).await;
        assert!(allowed.is_ok(), "{allowed:?}");
    }
}

#[cfg(feature = "bolt")]
mod over_bolt {
    use super::*;

    async fn serve_bolt(service: grafeo_service::ServiceState) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let backend = grafeo_boltr::GrafeoBackend::new(service).with_advertise_addr(addr);
        tokio::spawn(async move {
            grafeo_boltr::serve(backend, addr, grafeo_boltr::BoltrOptions::default())
                .await
                .unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        addr
    }

    async fn bolt_load(addr: SocketAddr, path: &str) -> Result<usize, String> {
        let mut session = boltr::client::BoltSession::connect(addr).await.unwrap();
        let result = session
            .run(&load_csv(path))
            .await
            .map(|r| r.records.len())
            .map_err(|e| e.to_string());
        let _ = session.close().await;
        result
    }

    #[tokio::test]
    async fn bolt_refuses_by_default_and_admits_the_admin_with_the_flag() {
        let (_dir, path) = people_csv();
        let refused = bolt_load(serve_bolt(open_service(false)).await, &path).await;
        let error = refused.expect_err("refused by default");
        assert!(error.contains("external access"), "{error}");
        assert!(error.contains("--allow-external-access"), "{error}");
        assert!(
            error.contains("Neo.ClientError.Security.Forbidden"),
            "a permission failure, not a statement error: {error}"
        );

        let allowed = bolt_load(serve_bolt(open_service(true)).await, &path).await;
        assert_eq!(allowed, Ok(2));
    }
}
