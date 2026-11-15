#![cfg(all(feature = "http", feature = "auth"))]
//! #90: the Studio logs in with the browser's Basic prompt, and a browser
//! sends those credentials only below the path where it was asked for them.
//! Opened at `/studio/`, the Studio's own API calls (`/db`, `/query`) went out
//! without them and got a 401 the browser could not answer, so the Studio
//! showed no databases. An API 401 now carries the Studio's challenge for a
//! same-origin browser request (or, on a plain-HTTP address where browsers
//! send no fetch metadata, a request with the Studio's marker header) when
//! Basic auth is configured, so the browser reuses the login it already has;
//! other requests keep a plain 401.

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
async fn the_studio_on_a_plain_http_address_gets_the_challenge() {
    // A browser sends no fetch metadata to an insecure origin (plain HTTP on
    // a LAN address or a host name), so the Studio's marker header is what
    // says the request is its own.
    let base = basic_server().await;
    let resp = Client::new()
        .get(format!("{base}/db"))
        .header("X-Grafeo-Studio", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(challenge_of(&resp).as_deref(), Some(CHALLENGE));
}

#[tokio::test]
async fn the_marker_does_not_override_fetch_metadata() {
    // Where the browser does say where a request comes from, that decides:
    // another site's page gets no prompt, marker or not.
    let base = basic_server().await;
    for site in ["cross-site", "same-site", "none"] {
        let resp = Client::new()
            .get(format!("{base}/db"))
            .header("Sec-Fetch-Site", site)
            .header("X-Grafeo-Studio", "1")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{site}");
        assert_eq!(challenge_of(&resp), None, "{site}");
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
    for marker in [("Sec-Fetch-Site", "same-origin"), ("X-Grafeo-Studio", "1")] {
        let resp = Client::new()
            .get(format!("{base}/db"))
            .header(marker.0, marker.1)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(challenge_of(&resp), None, "{marker:?}");
    }
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

/// The Studio's pages challenge only on a trusted name too: under any other
/// name the browser shows no prompt, and the 401 says which flag adds it.
#[tokio::test]
async fn studio_pages_challenge_only_on_trusted_names() {
    let state = grafeo_server::AppState::new(
        grafeo_service::ServiceState::new_in_memory_with_basic_auth(
            300,
            "admin".into(),
            "secret".into(),
        ),
        vec![],
        grafeo_service::types::EnabledFeatures::default(),
    );
    let studio = axum::Router::new().route("/studio/", axum::routing::get(|| async { "page" }));
    let app = grafeo_http::middleware::studio_auth::wrap_with_auth(studio, state);
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
    let port = addr.port();
    let page = |host: String| async move {
        Client::new()
            .get(format!("http://{addr}/studio/"))
            .header("host", host)
            .send()
            .await
            .unwrap()
    };

    let resp = page(format!("127.0.0.1:{port}")).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(challenge_of(&resp).as_deref(), Some(CHALLENGE));

    let resp = page(format!("rebound.example:{port}")).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(challenge_of(&resp), None, "no prompt for an untrusted name");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["detail"]
            .as_str()
            .is_some_and(|d| d.contains("--trusted-hosts")),
        "{body}"
    );
}
