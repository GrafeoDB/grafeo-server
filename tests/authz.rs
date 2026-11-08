#![cfg(all(feature = "http", feature = "auth"))]
//! Authorization over every HTTP route: a token reaches only the databases in
//! its scope, read-only tokens change nothing, read-write tokens administer
//! nothing (#82), and server-wide operations need an admin token with no
//! database list.
//!
//! The routes come from the running server's OpenAPI document plus the
//! undocumented ones listed in `UNDOCUMENTED`, and every route must sit in
//! one category, so a new route fails `every_route_is_classified` until it
//! is placed.

use std::net::SocketAddr;

use grafeo_service::auth::{Role, TokenRecord, TokenScope};
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};
use tokio::net::TcpListener;

const ROOT: &str = "root-token";
const ADMIN_A: &str = "admin-a-token";
const RW_A: &str = "rw-a-token";
const RW: &str = "rw-token";
const RO: &str = "ro-token";

/// Routes not in the OpenAPI document.
const UNDOCUMENTED: &[(&str, &str)] = &[
    ("GET", "/db/{name}/sparql"),
    ("POST", "/db/{name}/sparql"),
    ("GET", "/db/{name}/graph-store"),
    ("PUT", "/db/{name}/graph-store"),
    ("POST", "/db/{name}/graph-store"),
    ("DELETE", "/db/{name}/graph-store"),
    ("GET", "/ws"),
    ("GET", "/ready"),
    ("GET", "/metrics"),
];

/// Routes that name no database, need no admin, and check the database
/// they act on from the body or the session.
const BODY_OR_SESSION: &[&str] = &[
    "/query",
    "/cypher",
    "/graphql",
    "/gremlin",
    "/sparql",
    "/sql",
    "/batch",
    "/tx/begin",
    "/tx/query",
    "/tx/commit",
    "/tx/rollback",
    "/search/vector",
    "/search/text",
    "/search/hybrid",
    "/ws",
];

/// Server-wide operations: an admin token with no database list only.
const SERVER_WIDE: &[&str] = &[
    "/admin/tokens",
    "/admin/tokens/{id}",
    "/backups",
    "/admin/replication",
];

/// Open to any token (or exempt from auth).
const OPEN: &[&str] = &["/health", "/ready", "/metrics", "/system/resources", "/db"];

#[derive(Debug, PartialEq)]
enum Category {
    /// Names its database in the path (`/db/{name}/...`, `/admin/{db}/...`).
    PathDatabase,
    BodyOrSession,
    ServerWide,
    Open,
}

fn category(path: &str) -> Option<Category> {
    if SERVER_WIDE.contains(&path) {
        Some(Category::ServerWide)
    } else if path.starts_with("/db/{name}") || path.starts_with("/admin/{db}") {
        Some(Category::PathDatabase)
    } else if BODY_OR_SESSION.contains(&path) {
        Some(Category::BodyOrSession)
    } else if OPEN.contains(&path) {
        Some(Category::Open)
    } else {
        None
    }
}

fn token(plaintext: &str, name: &str, role: Role, databases: &[&str]) -> TokenRecord {
    TokenRecord {
        id: format!("tok-{name}"),
        name: name.to_string(),
        token_hash: grafeo_service::token_service::hash_token(plaintext),
        scope: TokenScope {
            role,
            databases: databases.iter().map(|d| (*d).to_string()).collect(),
        },
        created_at: "2026-01-01T00:00:00Z".to_string(),
        expires_at: None,
    }
}

/// A server with databases `a` and `b`, the bootstrap token `ROOT` and the
/// managed tokens above.
async fn spawn() -> String {
    spawn_with(false).await
}

/// As [`spawn`], with `/metrics` open to unauthenticated scrapes when
/// `public_metrics`.
async fn spawn_with(public_metrics: bool) -> String {
    let dir = tempfile::tempdir().unwrap();
    let store =
        grafeo_service::token_store::TokenStore::load(dir.path().join("tokens.json")).unwrap();
    for record in [
        token(ADMIN_A, "admin-a", Role::Admin, &["a"]),
        token(RW_A, "rw-a", Role::ReadWrite, &["a"]),
        token(RW, "rw", Role::ReadWrite, &[]),
        token(RO, "ro", Role::ReadOnly, &[]),
    ] {
        store.insert(record).unwrap();
    }
    let provider = grafeo_service::auth::AuthProvider::with_token_store(
        Some(ROOT.to_string()),
        None,
        None,
        std::sync::Arc::new(store),
    )
    .unwrap();
    let service = grafeo_service::ServiceState::new_in_memory_with_auth_provider(300, provider);
    let state = grafeo_server::AppState::new(
        service,
        vec![],
        grafeo_service::types::EnabledFeatures::default(),
    )
    .with_public_metrics(public_metrics);
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
    let base = format!("http://{addr}");
    for name in ["a", "b"] {
        let resp = Client::new()
            .post(format!("{base}/db"))
            .bearer_auth(ROOT)
            .json(&json!({ "name": name }))
            .send()
            .await
            .unwrap();
        assert!(
            resp.status().is_success(),
            "create {name}: {}",
            resp.status()
        );
    }
    base
}

/// Every route: the OpenAPI document's operations plus `UNDOCUMENTED`.
async fn routes(base: &str) -> Vec<(Method, String)> {
    let doc: Value = Client::new()
        .get(format!("{base}/api/openapi.json"))
        .bearer_auth(ROOT)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut out = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for method in ["get", "post", "put", "patch", "delete", "head"] {
            if item.get(method).is_some() {
                out.push((
                    Method::from_bytes(method.to_uppercase().as_bytes()).unwrap(),
                    path.clone(),
                ));
            }
        }
    }
    for (method, path) in UNDOCUMENTED {
        out.push((
            Method::from_bytes(method.as_bytes()).unwrap(),
            (*path).to_string(),
        ));
    }
    out
}

fn fill(path: &str, db: &str) -> String {
    path.replace("/db/{name}", &format!("/db/{db}"))
        .replace("{db}", db)
        .replace("{name}", "p1")
        .replace("{graph}", "g1")
        .replace("{schema}", "s1")
        .replace("{filename}", "b1.grafeo")
        .replace("{id}", "tok-rw")
}

/// A body the route accepts, aimed at database `db`.
fn body(path: &str, db: &str) -> Value {
    match path {
        "/batch" => json!({ "database": db, "queries": [{ "query": "MATCH (n) RETURN n" }] }),
        "/tx/begin" => json!({ "database": db }),
        "/search/vector" => json!({
            "database": db, "label": "L", "property": "e", "query_vector": [1.0], "k": 1
        }),
        "/search/text" => json!({
            "database": db, "label": "L", "property": "t", "query": "x", "k": 1
        }),
        "/search/hybrid" => json!({
            "database": db, "label": "L", "text_property": "t", "vector_property": "e",
            "query_text": "x", "k": 1
        }),
        _ => json!({ "database": db, "query": "MATCH (n) RETURN n" }),
    }
}

async fn send(
    base: &str,
    method: Method,
    path: &str,
    token: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let resp = Client::new()
        .request(method, format!("{base}{path}"))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let json = resp.json().await.unwrap_or(Value::Null);
    (status, json)
}

fn refused(status: StatusCode, body: &Value) -> bool {
    status == StatusCode::FORBIDDEN && body["error"] == "forbidden"
}

#[tokio::test]
async fn every_route_is_classified() {
    let base = spawn().await;
    let unclassified: Vec<_> = routes(&base)
        .await
        .into_iter()
        .filter(|(_, p)| category(p).is_none() && !p.starts_with("/api/"))
        .collect();
    assert!(
        unclassified.is_empty(),
        "place these routes in a category: {unclassified:?}"
    );
}

#[tokio::test]
async fn a_scoped_token_reaches_no_other_database() {
    let base = spawn().await;
    for (method, path) in routes(&base).await {
        let cat = category(&path);
        let target = match cat {
            Some(Category::PathDatabase) => fill(&path, "b"),
            Some(Category::BodyOrSession) if path.starts_with("/tx/") && path != "/tx/begin" => {
                continue;
            }
            Some(Category::BodyOrSession) if path == "/ws" => continue,
            Some(Category::BodyOrSession) => path.clone(),
            _ => continue,
        };
        let (status, json) = send(&base, method.clone(), &target, ADMIN_A, &body(&path, "b")).await;
        assert!(refused(status, &json), "{method} {target}: {status} {json}");
    }
}

#[tokio::test]
async fn a_scoped_token_reaches_its_own_database() {
    let base = spawn().await;
    for (method, path) in [
        (Method::GET, "/admin/a/stats"),
        (Method::GET, "/db/a"),
        (Method::POST, "/query"),
        (Method::POST, "/search/text"),
        (Method::POST, "/db/a/sparql"),
    ] {
        let (status, json) = send(&base, method.clone(), path, ADMIN_A, &body(path, "a")).await;
        assert!(!refused(status, &json), "{method} {path}: {status} {json}");
    }
}

#[tokio::test]
async fn creating_a_database_outside_the_scope_is_refused() {
    let base = spawn().await;
    let (status, json) = send(&base, Method::POST, "/db", ADMIN_A, &json!({ "name": "c" })).await;
    assert!(refused(status, &json), "{status} {json}");
}

#[tokio::test]
async fn server_wide_operations_need_an_unscoped_admin() {
    let base = spawn().await;
    for (method, path) in routes(&base).await {
        if category(&path) != Some(Category::ServerWide) {
            continue;
        }
        let target = fill(&path, "a");
        let (status, json) = send(&base, method.clone(), &target, ADMIN_A, &json!({})).await;
        assert!(refused(status, &json), "{method} {target}: {status} {json}");
        let (status, json) = send(&base, method.clone(), &target, ROOT, &json!({})).await;
        assert!(
            !refused(status, &json),
            "root: {method} {target}: {status} {json}"
        );
    }
}

#[tokio::test]
async fn read_only_tokens_change_nothing() {
    let base = spawn().await;
    for (method, path) in routes(&base).await {
        let is_write = matches!(
            method,
            Method::POST | Method::PUT | Method::PATCH | Method::DELETE
        );
        // Queries run with a read-only engine session, which refuses writes itself.
        if !is_write
            || path == "/db/{name}/sparql"
            || category(&path) == Some(Category::BodyOrSession)
        {
            continue;
        }
        if category(&path) == Some(Category::Open) && path != "/db" {
            continue;
        }
        let target = fill(&path, "a");
        let (status, json) =
            send(&base, method.clone(), &target, RO, &json!({ "name": "c" })).await;
        assert!(refused(status, &json), "{method} {target}: {status} {json}");
    }
}

#[tokio::test]
async fn read_write_tokens_administer_nothing() {
    let base = spawn().await;
    for (method, path) in routes(&base).await {
        let admin = path.starts_with("/admin/")
            || SERVER_WIDE.contains(&path.as_str())
            || (path == "/db" && method == Method::POST)
            || (path == "/db/{name}" && method == Method::DELETE);
        if !admin {
            continue;
        }
        let target = fill(&path, "a");
        for tok in [RW, RW_A] {
            let (status, json) =
                send(&base, method.clone(), &target, tok, &json!({ "name": "c" })).await;
            assert!(
                refused(status, &json),
                "{tok}: {method} {target}: {status} {json}"
            );
        }
    }
}

#[tokio::test]
async fn read_write_tokens_still_write_data() {
    let base = spawn().await;
    let (status, json) = send(
        &base,
        Method::POST,
        "/db/a/graphs",
        RW_A,
        &json!({ "name": "g1" }),
    )
    .await;
    assert!(status.is_success(), "{status} {json}");
    let (status, json) = send(
        &base,
        Method::POST,
        "/query",
        RW_A,
        &json!({ "database": "a", "query": "INSERT (:N {v: 1})" }),
    )
    .await;
    assert!(status.is_success(), "{status} {json}");
}

#[tokio::test]
async fn a_cross_database_restore_needs_both_databases_in_scope() {
    let base = spawn().await;
    let (status, json) = send(
        &base,
        Method::POST,
        "/admin/a/restore",
        ADMIN_A,
        &json!({ "backup": "b1.grafeo", "source_db": "b" }),
    )
    .await;
    assert!(refused(status, &json), "{status} {json}");
}

#[tokio::test]
async fn metrics_need_credentials_when_auth_is_on() {
    let base = spawn().await;
    let anonymous = Client::new()
        .get(format!("{base}/metrics"))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let scraper = Client::new()
        .get(format!("{base}/metrics"))
        .bearer_auth(RO)
        .send()
        .await
        .unwrap();
    assert_eq!(scraper.status(), StatusCode::OK);
    let health = Client::new()
        .get(format!("{base}/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        health.status(),
        StatusCode::OK,
        "/health stays open for probes"
    );
}

#[tokio::test]
async fn public_metrics_keep_metrics_open() {
    let base = spawn_with(true).await;
    let anonymous = Client::new()
        .get(format!("{base}/metrics"))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_percent_encoded_path_is_authorized_as_the_decoded_database() {
    // `/admin/%61/stats` decodes to database `a` in the handler, so the scope
    // check must compare the decoded name: ADMIN_A (scoped to `a`) passes,
    // and a token scoped to the literal `%61` would not reach `a`.
    let base = spawn().await;
    let client = Client::new();
    let resp = client
        .get(format!("{base}/admin/%61/stats"))
        .header("Authorization", format!("Bearer {ADMIN_A}"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "ADMIN_A scoped to `a` reaches /admin/%61/stats: {}",
        resp.text().await.unwrap()
    );

    // The other direction stays closed: a read-only token scoped to `a`
    // cannot use the encoding to reach another database.
    let resp = client
        .get(format!("{base}/admin/%62/stats"))
        .header("Authorization", format!("Bearer {ADMIN_A}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}
