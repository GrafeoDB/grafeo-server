#![cfg(all(feature = "http", feature = "auth"))]
//! #90: the Studio logs in with the browser's Basic prompt, and a browser
//! sends those credentials only below the path where it was asked for them.
//! Opened at `/studio/`, the Studio's own API calls (`/db`, `/query`) went out
//! without them and got a 401 the browser could not answer, so the Studio
//! showed no databases. An API 401 now carries the Studio's challenge for a
//! same-origin browser request when Basic auth is configured, so the browser
//! reuses the login it already has; other requests keep a plain 401.

use std::net::SocketAddr;

use reqwest::{Client, StatusCode};
use serde_json::json;
use tokio::net::TcpListener;

const CHALLENGE: &str = r#"Basic realm="Grafeo Studio", charset="UTF-8""#;

async fn serve(service: grafeo_service::ServiceState) -> String {
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
    format!("http://{addr}")
}

async fn basic_server() -> String {
    serve(grafeo_service::ServiceState::new_in_memory_with_basic_auth(
        300,
        "admin".into(),
        "secret".into(),
    ))
    .await
}

fn challenge_of(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get(reqwest::header::WWW_AUTHENTICATE)
        .map(|v| v.to_str().unwrap().to_string())
}

#[tokio::test]
async fn a_same_origin_api_401_carries_the_studio_challenge() {
    let base = basic_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/db"))
        .header("Sec-Fetch-Site", "same-origin")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(challenge_of(&resp).as_deref(), Some(CHALLENGE));

    let resp = client
        .post(format!("{base}/query"))
        .header("Sec-Fetch-Site", "same-origin")
        .json(&json!({"query": "MATCH (n) RETURN n"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(challenge_of(&resp).as_deref(), Some(CHALLENGE));
}

#[tokio::test]
async fn wrong_credentials_get_the_challenge_again() {
    let base = basic_server().await;
    let resp = Client::new()
        .get(format!("{base}/db"))
        .header("Sec-Fetch-Site", "same-origin")
        .basic_auth("admin", Some("wrong"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(challenge_of(&resp).as_deref(), Some(CHALLENGE));
}

#[tokio::test]
async fn other_requests_keep_a_plain_401() {
    let base = basic_server().await;
    let client = Client::new();
    // Another site's page, a navigation typed by the user, a sibling
    // subdomain, and a script or tool that sends no fetch metadata: none of
    // them is the Studio, so none gets a login prompt.
    for site in [Some("cross-site"), Some("none"), Some("same-site"), None] {
        let mut request = client.get(format!("{base}/db"));
        if let Some(site) = site {
            request = request.header("Sec-Fetch-Site", site);
        }
        let resp = request.send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{site:?}");
        assert_eq!(challenge_of(&resp), None, "{site:?}");
    }
}

#[tokio::test]
async fn a_token_only_server_sends_no_basic_challenge() {
    // Basic credentials could never succeed there, so a prompt would only
    // trap the user.
    let base = serve(grafeo_service::ServiceState::new_in_memory_with_auth(
        300,
        "token".into(),
    ))
    .await;
    let resp = Client::new()
        .get(format!("{base}/db"))
        .header("Sec-Fetch-Site", "same-origin")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(challenge_of(&resp), None);
}

#[tokio::test]
async fn the_right_credentials_still_pass_and_health_stays_open() {
    let base = basic_server().await;
    let client = Client::new();
    let resp = client
        .get(format!("{base}/db"))
        .header("Sec-Fetch-Site", "same-origin")
        .basic_auth("admin", Some("secret"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(challenge_of(&resp), None);

    let resp = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
