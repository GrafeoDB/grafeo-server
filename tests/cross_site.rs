#![cfg(feature = "http")]
//! Cross-site request refusal: a browser page on another site can't write
//! through the HTTP API or open the WebSocket, whatever the route, the auth
//! setup or `--cors-origins`. Non-browser clients (no `Origin`, no
//! `Sec-Fetch-Site`) and same-origin pages (Studio) are unaffected.

use std::net::SocketAddr;

use reqwest::{Client, Method, StatusCode};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const EVIL: &str = "https://evil.example";

async fn serve(state: grafeo_server::AppState) -> String {
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

async fn spawn_default() -> String {
    serve(grafeo_server::AppState::new_in_memory(300)).await
}

async fn spawn_with_cors(origins: &[&str]) -> String {
    let state = grafeo_server::AppState::new(
        grafeo_server::grafeo_service::ServiceState::new_in_memory(300),
        origins.iter().map(|o| (*o).to_string()).collect(),
        grafeo_server::grafeo_service::types::EnabledFeatures::default(),
    );
    serve(state).await
}

/// Every write operation the OpenAPI document lists, with its path
/// parameters filled in. Read from the running server, so a new write route
/// is covered without a change here.
async fn write_operations(base: &str) -> Vec<(Method, String)> {
    let doc: Value = Client::new()
        .get(format!("{base}/api/openapi.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut ops = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for method in ["post", "put", "patch", "delete"] {
            if item.get(method).is_some() {
                let filled = path
                    .replace("{name}", "default")
                    .replace("{db}", "default")
                    .replace("{graph}", "g1")
                    .replace("{schema}", "s1")
                    .replace("{filename}", "b1.grafeo")
                    .replace("{id}", "tok-1");
                assert!(!filled.contains('{'), "unfilled parameter in {path}");
                ops.push((
                    Method::from_bytes(method.to_uppercase().as_bytes()).unwrap(),
                    filled,
                ));
            }
        }
    }
    ops
}

fn is_cross_site_refusal(status: StatusCode, body: &Value) -> bool {
    status == StatusCode::FORBIDDEN && body["error"] == "cross_site_request"
}

async fn send(
    base: &str,
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut req = Client::new()
        .request(method, format!("{base}{path}"))
        .header("content-type", "application/json")
        .body("{}");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status();
    let body = resp.json().await.unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn every_write_route_refuses_a_cross_site_request() {
    let base = spawn_default().await;
    let ops = write_operations(&base).await;
    assert!(
        ops.len() >= 30,
        "only {} write operations listed",
        ops.len()
    );
    for (method, path) in ops {
        let (status, body) = send(
            &base,
            method.clone(),
            &path,
            &[("origin", EVIL), ("sec-fetch-site", "cross-site")],
        )
        .await;
        assert!(
            is_cross_site_refusal(status, &body),
            "{method} {path}: {status} {body}"
        );
    }
}

#[tokio::test]
async fn an_origin_without_fetch_metadata_is_checked_against_the_host() {
    let base = spawn_default().await;
    let ops = write_operations(&base).await;
    for (method, path) in ops {
        let (status, body) = send(&base, method.clone(), &path, &[("origin", EVIL)]).await;
        assert!(
            is_cross_site_refusal(status, &body),
            "{method} {path}: {status} {body}"
        );
    }
}

#[tokio::test]
async fn a_cross_site_sparql_form_update_changes_nothing() {
    let base = spawn_default().await;
    let client = Client::new();
    let resp = client
        .post(format!("{base}/db/default/sparql"))
        .header("origin", EVIL)
        .header("sec-fetch-site", "cross-site")
        .header("content-type", "application/x-www-form-urlencoded")
        .body("update=INSERT%20DATA%20%7B%20%3Chttp%3A%2F%2Fa%3E%20%3Chttp%3A%2F%2Fb%3E%20%3Chttp%3A%2F%2Fc%3E%20%7D")
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body: Value = resp.json().await.unwrap();
    assert!(is_cross_site_refusal(status, &body), "{status} {body}");
}

#[tokio::test]
async fn same_site_but_other_origin_is_refused() {
    // Another port on the same host is same-site, not same-origin.
    let base = spawn_default().await;
    let (status, body) = send(
        &base,
        Method::POST,
        "/query",
        &[
            ("origin", "http://127.0.0.1:1"),
            ("sec-fetch-site", "same-site"),
        ],
    )
    .await;
    assert!(is_cross_site_refusal(status, &body), "{status} {body}");
}

#[tokio::test]
async fn same_origin_and_non_browser_requests_pass() {
    let base = spawn_default().await;
    let own_origin = base.clone();
    let cases: [&[(&str, &str)]; 4] = [
        &[],
        &[("sec-fetch-site", "same-origin"), ("origin", &own_origin)],
        &[("sec-fetch-site", "none")],
        &[("origin", &own_origin)],
    ];
    for headers in cases {
        let (status, body) = send(&base, Method::POST, "/query", headers).await;
        assert!(
            !is_cross_site_refusal(status, &body),
            "{headers:?}: {status} {body}"
        );
    }
}

#[tokio::test]
async fn studio_behind_a_host_rewriting_proxy_passes_on_fetch_metadata() {
    // A proxy may rewrite Host, so Origin no longer matches it; the browser's
    // Sec-Fetch-Site still says same-origin.
    let base = spawn_default().await;
    let (status, body) = send(
        &base,
        Method::POST,
        "/query",
        &[
            ("origin", "https://graph.example.org"),
            ("sec-fetch-site", "same-origin"),
        ],
    )
    .await;
    assert!(!is_cross_site_refusal(status, &body), "{status} {body}");
}

#[tokio::test]
async fn a_listed_cors_origin_may_write() {
    let base = spawn_with_cors(&["https://app.example"]).await;
    let (status, body) = send(
        &base,
        Method::POST,
        "/query",
        &[
            ("origin", "https://app.example"),
            ("sec-fetch-site", "cross-site"),
        ],
    )
    .await;
    assert!(!is_cross_site_refusal(status, &body), "{status} {body}");

    let (status, body) = send(
        &base,
        Method::POST,
        "/query",
        &[("origin", EVIL), ("sec-fetch-site", "cross-site")],
    )
    .await;
    assert!(is_cross_site_refusal(status, &body), "{status} {body}");
}

#[tokio::test]
async fn cross_site_reads_are_left_to_cors() {
    let base = spawn_default().await;
    let (status, _) = send(
        &base,
        Method::GET,
        "/health",
        &[("origin", EVIL), ("sec-fetch-site", "cross-site")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

fn ws_request(base: &str, headers: &[(&str, &str)]) -> tungstenite::handshake::client::Request {
    let mut req = format!("{}/ws", base.replace("http://", "ws://"))
        .into_client_request()
        .unwrap();
    for (k, v) in headers {
        req.headers_mut().insert(
            tungstenite::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            v.parse().unwrap(),
        );
    }
    req
}

fn ws_refused_with_403(result: Result<impl Sized, tungstenite::Error>) -> bool {
    matches!(result, Err(tungstenite::Error::Http(resp)) if resp.status() == 403)
}

#[tokio::test]
async fn websocket_refuses_a_cross_site_page_by_default() {
    let base = spawn_default().await;
    let result = tokio_tungstenite::connect_async(ws_request(&base, &[("origin", EVIL)])).await;
    assert!(ws_refused_with_403(result.map(|_| ())));

    let result = tokio_tungstenite::connect_async(ws_request(
        &base,
        &[("origin", EVIL), ("sec-fetch-site", "cross-site")],
    ))
    .await;
    assert!(ws_refused_with_403(result.map(|_| ())));
}

#[tokio::test]
async fn websocket_accepts_same_origin_listed_origin_and_non_browser_clients() {
    let base = spawn_with_cors(&["https://app.example"]).await;
    let own_origin = base.clone();
    let cases: [&[(&str, &str)]; 3] = [
        &[],
        &[("origin", &own_origin)],
        &[("origin", "https://app.example")],
    ];
    for headers in cases {
        let result = tokio_tungstenite::connect_async(ws_request(&base, headers)).await;
        assert!(result.is_ok(), "{headers:?}: {:?}", result.err());
    }
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn websocket_refuses_a_cross_site_page_even_with_cached_basic_auth() {
    use base64::Engine as _;
    let base = serve(grafeo_server::AppState::new_in_memory_with_basic_auth(
        300,
        "admin".to_string(),
        "secret".to_string(),
    ))
    .await;
    let basic = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("admin:secret")
    );
    let result = tokio_tungstenite::connect_async(ws_request(
        &base,
        &[("origin", EVIL), ("authorization", &basic)],
    ))
    .await;
    assert!(ws_refused_with_403(result.map(|_| ())));

    // The same credentials from a non-browser client still connect.
    let result =
        tokio_tungstenite::connect_async(ws_request(&base, &[("authorization", &basic)])).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn same_origin_with_an_untrusted_host_is_refused() {
    // DNS rebinding: the rebound page's own name is in Host and it sends
    // Sec-Fetch-Site: same-origin, but the server does not answer on that
    // name, so the header alone must not pass.
    let base = spawn_default().await;
    let (status, body) = send(
        &base,
        Method::POST,
        "/query",
        &[
            ("host", "evil.example:7474"),
            ("origin", "http://evil.example:7474"),
            ("sec-fetch-site", "same-origin"),
        ],
    )
    .await;
    assert!(is_cross_site_refusal(status, &body), "{status} {body}");
}

#[tokio::test]
async fn an_origin_on_another_scheme_is_not_the_servers_own() {
    // The server is plain http; an https origin with the same authority is
    // not this server's own origin.
    let base = spawn_default().await;
    let authority = base.trim_start_matches("http://");
    let https_origin = format!("https://{authority}");
    let (status, body) = send(&base, Method::POST, "/query", &[("origin", &https_origin)]).await;
    assert!(is_cross_site_refusal(status, &body), "{status} {body}");

    // The same authority on the server's own scheme passes.
    let (status, body) = send(&base, Method::POST, "/query", &[("origin", &base)]).await;
    assert!(!is_cross_site_refusal(status, &body), "{status} {body}");
}

#[tokio::test]
async fn a_refused_cross_site_request_still_carries_a_request_id() {
    let base = spawn_default().await;
    let resp = Client::new()
        .post(format!("{base}/query"))
        .header("origin", EVIL)
        .header("sec-fetch-site", "cross-site")
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(
        resp.headers().get("x-request-id").is_some(),
        "the 403 must carry an X-Request-Id header"
    );
}
