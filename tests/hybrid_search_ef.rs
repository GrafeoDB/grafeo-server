#![cfg(all(feature = "hybrid-search", feature = "http"))]
//! `ef` on hybrid search (grafeo-server#85) over the transports: `/search/hybrid`
//! takes it and decodes it strictly, it is in the OpenAPI schema, and HTTP and
//! GWP pass it to the vector half, on a vector index with a narrow
//! construction beam where the default search beam misses exact neighbours.

use std::net::SocketAddr;
use std::sync::OnceLock;

use grafeo_common::Value as GrafeoValue;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tokio::net::TcpListener;

const DIMENSIONS: usize = 16;
const NODES: usize = 3000;
const K: usize = 5;
/// A text that no document contains, so the vector half alone decides.
const NO_TEXT_MATCH: &str = "zzzz";
const SEEDS: std::ops::Range<u64> = 1..9;

/// A linear congruential generator, so every run builds the same vectors.
struct Lcg(u64);

impl Lcg {
    fn next_value(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        f32::from(u16::try_from(self.0 >> 48).expect("16 bits")) / 65_536.0
    }

    fn vector(&mut self) -> Vec<f32> {
        (0..DIMENSIONS).map(|_| self.next_value()).collect()
    }
}

fn query_vector(seed: u64) -> Vec<f32> {
    Lcg(seed).vector()
}

/// The fixture's vectors by node id, built once for every test.
fn vectors() -> &'static Vec<(u64, Vec<f32>)> {
    static VECTORS: OnceLock<Vec<(u64, Vec<f32>)>> = OnceLock::new();
    VECTORS.get_or_init(|| {
        let mut values = Lcg(0x5EED);
        (0..NODES).map(|_| (0, values.vector())).collect()
    })
}

/// A service whose `default` database holds the fixture; returns the node ids
/// in creation order.
fn seeded_service() -> (grafeo_service::ServiceState, Vec<u64>) {
    let service = grafeo_service::ServiceState::new_in_memory(300);
    let db = service.databases().get_available("default").unwrap().db();
    let mut ids = Vec::with_capacity(NODES);
    for (i, (_, vector)) in vectors().iter().enumerate() {
        let id = db.create_node(&["Doc"]).unwrap();
        let text = if i % 3 == 0 {
            "Lisbon harbour at dawn"
        } else {
            "Porto bridges at night"
        };
        db.set_node_property(id, "content", GrafeoValue::String(text.into()))
            .unwrap();
        db.set_node_property(id, "emb", GrafeoValue::Vector(vector.clone().into()))
            .unwrap();
        ids.push(id.as_u64());
    }
    db.create_text_index("Doc", "content").unwrap();
    db.create_vector_index(
        "Doc",
        "emb",
        Some(DIMENSIONS),
        Some("euclidean"),
        Some(16),
        Some(8),
        None,
    )
    .unwrap();
    (service, ids)
}

/// The `k` nodes nearest to `query` by a scan of every vector, sorted by id.
fn exact_nearest(ids: &[u64], query: &[f32], k: usize) -> Vec<u64> {
    let mut by_distance: Vec<(f32, u64)> = vectors()
        .iter()
        .zip(ids)
        .map(|((_, v), id)| {
            let d: f32 = v.iter().zip(query).map(|(a, b)| (a - b) * (a - b)).sum();
            (d, *id)
        })
        .collect();
    by_distance.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut nearest: Vec<u64> = by_distance.into_iter().take(k).map(|(_, id)| id).collect();
    nearest.sort_unstable();
    nearest
}

struct Servers {
    http: String,
    #[cfg(feature = "gwp")]
    gwp: String,
    ids: Vec<u64>,
}

async fn spawn() -> Servers {
    use grafeo_service::types::EnabledFeatures;

    let (service, ids) = seeded_service();
    let features = EnabledFeatures {
        languages: vec![],
        engine: vec![],
        server: vec![],
    };
    let state = grafeo_server::AppState::new(service.clone(), vec![], features);
    let app = grafeo_server::router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    #[cfg(feature = "gwp")]
    let gwp = {
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
    };

    Servers {
        http: format!("http://{http_addr}"),
        #[cfg(feature = "gwp")]
        gwp,
        ids,
    }
}

fn hybrid_body(query_text: &str, query: &[f32], ef: Option<u32>) -> Value {
    let mut body = json!({
        "label": "Doc",
        "text_property": "content",
        "vector_property": "emb",
        "query_text": query_text,
        "query_vector": query,
        "k": K,
    });
    if let Some(ef) = ef {
        body["ef"] = json!(ef);
    }
    body
}

/// The node ids of an HTTP hybrid search, in result order.
async fn http_hybrid(client: &Client, base: &str, body: &Value) -> Vec<u64> {
    let resp = client
        .post(format!("{base}/search/hybrid"))
        .json(body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    body["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["node_id"].as_u64().unwrap())
        .collect()
}

fn sorted(mut ids: Vec<u64>) -> Vec<u64> {
    ids.sort_unstable();
    ids
}

#[tokio::test]
async fn http_full_beam_finds_the_exact_nearest_neighbours() {
    let servers = spawn().await;
    let client = Client::new();
    let full = u32::try_from(NODES).unwrap();
    let mut default_missed = 0;
    for seed in SEEDS {
        let query = query_vector(seed);
        let exact = exact_nearest(&servers.ids, &query, K);
        let at_full = http_hybrid(
            &client,
            &servers.http,
            &hybrid_body(NO_TEXT_MATCH, &query, Some(full)),
        )
        .await;
        assert_eq!(sorted(at_full), exact, "seed {seed}");
        let at_default = http_hybrid(
            &client,
            &servers.http,
            &hybrid_body(NO_TEXT_MATCH, &query, None),
        )
        .await;
        if sorted(at_default) != exact {
            default_missed += 1;
        }
    }
    assert!(default_missed > 0, "the fixture no longer shows ef");
}

#[tokio::test]
async fn http_ef_absent_null_and_set_are_accepted() {
    let servers = spawn().await;
    let client = Client::new();
    let query = query_vector(1);

    let absent = http_hybrid(
        &client,
        &servers.http,
        &hybrid_body("harbour", &query, None),
    )
    .await;
    let mut null = hybrid_body("harbour", &query, None);
    null["ef"] = Value::Null;
    let null = http_hybrid(&client, &servers.http, &null).await;
    assert_eq!(absent, null, "ef null is the same as no ef");
    assert_eq!(absent.len(), K);

    let set = http_hybrid(
        &client,
        &servers.http,
        &hybrid_body("harbour", &query, Some(64)),
    )
    .await;
    assert_eq!(set.len(), K);
}

#[tokio::test]
async fn http_rejects_an_ef_that_is_not_a_u32() {
    let servers = spawn().await;
    let client = Client::new();
    for bad in [
        json!(-1),
        json!("64"),
        json!(1.5),
        json!(4_294_967_296_u64),
        json!([64]),
    ] {
        let mut body = hybrid_body("harbour", &query_vector(1), None);
        body["ef"] = bad.clone();
        let resp = client
            .post(format!("{}/search/hybrid", servers.http))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(
            resp.status().is_client_error(),
            "ef {bad}: got {}",
            resp.status()
        );
        let text = resp.text().await.unwrap();
        assert!(
            text.contains("ef"),
            "ef {bad}: the error names the field: {text}"
        );
    }
}

#[tokio::test]
async fn openapi_lists_ef_on_the_hybrid_request() {
    let servers = spawn().await;
    let body: Value = Client::new()
        .get(format!("{}/api/openapi.json", servers.http))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let schema = &body["components"]["schemas"]["HybridSearchReq"];
    let ef = &schema["properties"]["ef"];
    assert!(!ef.is_null(), "HybridSearchReq has ef: {schema}");
    let required: Vec<&str> = schema["required"]
        .as_array()
        .map_or_default(|r| r.iter().filter_map(Value::as_str).collect());
    assert!(!required.contains(&"ef"), "ef is optional: {required:?}");
}

#[cfg(feature = "gwp")]
mod over_gwp {
    use super::*;
    use gwp::client::GqlConnection;
    use gwp::server::HybridSearchParams;

    async fn gwp_hybrid(
        endpoint: &str,
        query_text: &str,
        query: &[f32],
        ef: Option<u32>,
    ) -> Vec<u64> {
        let conn = GqlConnection::connect(endpoint).await.unwrap();
        let mut search = conn.create_search_client();
        let params = HybridSearchParams::new(
            "default",
            "Doc",
            "content",
            "emb",
            query_text,
            u32::try_from(K).unwrap(),
        )
        .with_query_vector(query.to_vec())
        .with_ef(ef);
        search
            .hybrid_search(params)
            .await
            .unwrap()
            .into_iter()
            .map(|h| h.node_id)
            .collect()
    }

    #[tokio::test]
    async fn gwp_full_beam_finds_the_exact_nearest_neighbours() {
        let servers = spawn().await;
        let full = u32::try_from(NODES).unwrap();
        for seed in SEEDS {
            let query = query_vector(seed);
            let exact = exact_nearest(&servers.ids, &query, K);
            let at_full = gwp_hybrid(&servers.gwp, NO_TEXT_MATCH, &query, Some(full)).await;
            assert_eq!(sorted(at_full), exact, "seed {seed}");
        }
    }

    #[tokio::test]
    async fn gwp_and_http_return_the_same_results_for_each_ef() {
        let servers = spawn().await;
        let client = Client::new();
        for ef in [None, Some(8), Some(64), Some(400)] {
            for seed in SEEDS {
                let query = query_vector(seed);
                for text in ["harbour", NO_TEXT_MATCH] {
                    let over_http =
                        http_hybrid(&client, &servers.http, &hybrid_body(text, &query, ef)).await;
                    let over_gwp = gwp_hybrid(&servers.gwp, text, &query, ef).await;
                    assert_eq!(over_gwp, over_http, "seed {seed}, ef {ef:?}, text {text}");
                }
            }
        }
    }
}
