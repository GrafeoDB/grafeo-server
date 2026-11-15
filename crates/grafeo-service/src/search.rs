//! Search operations — vector, text, and hybrid search.
//!
//! Transport-agnostic. Called by both HTTP routes and GWP backend.
//! Feature-gated: requires `vector-index`, `text-index`, or `hybrid-search`.

use crate::database::DatabaseManager;
use crate::error::ServiceError;
use crate::types;

/// Stateless search operations.
pub struct SearchService;

impl SearchService {
    /// Vector similarity search (KNN via HNSW index).
    #[cfg(feature = "vector-index")]
    pub async fn vector_search(
        databases: &DatabaseManager,
        db_name: &str,
        req: types::VectorSearchReq,
    ) -> Result<Vec<types::SearchHit>, ServiceError> {
        let entry = databases.get_available(db_name)?;

        let results = tokio::task::spawn_blocking(move || {
            let filters = if req.filters.is_empty() {
                None
            } else {
                Some(req.filters)
            };
            entry.db().vector_search(
                &req.label,
                &req.property,
                &req.query_vector,
                req.k as usize,
                req.ef.map(|v| v as usize),
                filters.as_ref(),
            )
        })
        .await
        .map_err(|e| ServiceError::Internal(e.to_string()))?
        .map_err(|e| ServiceError::BadRequest(e.to_string()))?;

        Ok(results
            .into_iter()
            .map(|(node_id, distance)| types::SearchHit {
                node_id: node_id.as_u64(),
                score: f64::from(distance),
                properties: std::collections::HashMap::new(),
            })
            .collect())
    }

    /// Vector search stub when feature is disabled.
    #[cfg(not(feature = "vector-index"))]
    pub fn vector_search(
        _databases: &DatabaseManager,
        _db_name: &str,
        _req: types::VectorSearchReq,
    ) -> impl Future<Output = Result<Vec<types::SearchHit>, ServiceError>> {
        std::future::ready(Err(ServiceError::BadRequest(
            "vector-index feature not enabled".to_owned(),
        )))
    }

    /// Full-text search (BM25 scoring).
    #[cfg(feature = "text-index")]
    pub async fn text_search(
        databases: &DatabaseManager,
        db_name: &str,
        req: types::TextSearchReq,
    ) -> Result<Vec<types::SearchHit>, ServiceError> {
        let entry = databases.get_available(db_name)?;

        let results = tokio::task::spawn_blocking(move || {
            entry
                .db()
                .text_search(&req.label, &req.property, &req.query, req.k as usize, None)
        })
        .await
        .map_err(|e| ServiceError::Internal(e.to_string()))?
        .map_err(|e| ServiceError::BadRequest(e.to_string()))?;

        Ok(results
            .into_iter()
            .map(|(node_id, score)| types::SearchHit {
                node_id: node_id.as_u64(),
                score,
                properties: std::collections::HashMap::new(),
            })
            .collect())
    }

    /// Text search stub when feature is disabled.
    #[cfg(not(feature = "text-index"))]
    pub fn text_search(
        _databases: &DatabaseManager,
        _db_name: &str,
        _req: types::TextSearchReq,
    ) -> impl Future<Output = Result<Vec<types::SearchHit>, ServiceError>> {
        std::future::ready(Err(ServiceError::BadRequest(
            "text-index feature not enabled".to_owned(),
        )))
    }

    /// Hybrid search (vector + text with rank fusion).
    #[cfg(feature = "hybrid-search")]
    pub async fn hybrid_search(
        databases: &DatabaseManager,
        db_name: &str,
        req: types::HybridSearchReq,
    ) -> Result<Vec<types::SearchHit>, ServiceError> {
        let entry = databases.get_available(db_name)?;

        let results = tokio::task::spawn_blocking(move || {
            let mut options = grafeo_engine::database::HybridSearchOptions::new();
            if !req.query_vector.is_empty() {
                options = options.with_query_vector(&req.query_vector);
            }
            if let Some(ef) = req.ef {
                options = options.with_ef(ef as usize);
            }
            entry.db().hybrid_search_with(
                &req.label,
                &req.text_property,
                &req.vector_property,
                &req.query_text,
                req.k as usize,
                &options,
            )
        })
        .await
        .map_err(|e| ServiceError::Internal(e.to_string()))?
        .map_err(|e| ServiceError::BadRequest(e.to_string()))?;

        Ok(results
            .into_iter()
            .map(|(node_id, score)| types::SearchHit {
                node_id: node_id.as_u64(),
                score,
                properties: std::collections::HashMap::new(),
            })
            .collect())
    }

    /// Hybrid search stub when feature is disabled.
    #[cfg(not(feature = "hybrid-search"))]
    pub fn hybrid_search(
        _databases: &DatabaseManager,
        _db_name: &str,
        _req: types::HybridSearchReq,
    ) -> impl Future<Output = Result<Vec<types::SearchHit>, ServiceError>> {
        std::future::ready(Err(ServiceError::BadRequest(
            "hybrid-search feature not enabled".to_owned(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::ServiceState;

    fn state() -> ServiceState {
        ServiceState::new_in_memory(300)
    }

    // -----------------------------------------------------------------------
    // vector_search
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn vector_search_not_found_database() {
        let s = state();
        let req = types::VectorSearchReq {
            database: "default".into(),
            label: "Node".into(),
            property: "embedding".into(),
            query_vector: vec![1.0, 2.0, 3.0],
            k: 5,
            ef: None,
            filters: HashMap::default(),
        };
        let err = SearchService::vector_search(s.databases(), "no_such_db", req)
            .await
            .unwrap_err();
        // Without vector-index feature: BadRequest; with it: NotFound
        assert!(matches!(
            err,
            ServiceError::NotFound(_) | ServiceError::BadRequest(_)
        ));
    }

    #[cfg(not(feature = "vector-index"))]
    #[tokio::test]
    async fn vector_search_feature_disabled() {
        let s = state();
        let req = types::VectorSearchReq {
            database: "default".into(),
            label: "Node".into(),
            property: "embedding".into(),
            query_vector: vec![1.0],
            k: 5,
            ef: None,
            filters: HashMap::default(),
        };
        let err = SearchService::vector_search(s.databases(), "default", req)
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::BadRequest(_)));
        assert!(err.to_string().contains("vector-index"));
    }

    // -----------------------------------------------------------------------
    // text_search
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn text_search_not_found_database() {
        let s = state();
        let req = types::TextSearchReq {
            database: "default".into(),
            label: "Doc".into(),
            property: "content".into(),
            query: "hello".into(),
            k: 10,
        };
        let err = SearchService::text_search(s.databases(), "missing_db", req)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ServiceError::NotFound(_) | ServiceError::BadRequest(_)
        ));
    }

    #[cfg(not(feature = "text-index"))]
    #[tokio::test]
    async fn text_search_feature_disabled() {
        let s = state();
        let req = types::TextSearchReq {
            database: "default".into(),
            label: "Doc".into(),
            property: "content".into(),
            query: "hello".into(),
            k: 10,
        };
        let err = SearchService::text_search(s.databases(), "default", req)
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::BadRequest(_)));
        assert!(err.to_string().contains("text-index"));
    }

    // -----------------------------------------------------------------------
    // hybrid_search
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn hybrid_search_not_found_database() {
        let s = state();
        let req = types::HybridSearchReq {
            database: "default".into(),
            label: "Doc".into(),
            text_property: "content".into(),
            vector_property: "embedding".into(),
            query_text: "hello".into(),
            query_vector: vec![1.0, 2.0],
            k: 5,
            ef: None,
        };
        let err = SearchService::hybrid_search(s.databases(), "nope", req)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ServiceError::NotFound(_) | ServiceError::BadRequest(_)
        ));
    }

    #[cfg(not(feature = "hybrid-search"))]
    #[tokio::test]
    async fn hybrid_search_feature_disabled() {
        let s = state();
        let req = types::HybridSearchReq {
            database: "default".into(),
            label: "Doc".into(),
            text_property: "content".into(),
            vector_property: "embedding".into(),
            query_text: "hello".into(),
            query_vector: vec![1.0],
            k: 5,
            ef: None,
        };
        let err = SearchService::hybrid_search(s.databases(), "default", req)
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::BadRequest(_)));
        assert!(err.to_string().contains("hybrid-search"));
    }

    #[test]
    fn hybrid_search_req_decodes_ef() {
        let base = serde_json::json!({
            "label": "Doc",
            "text_property": "content",
            "vector_property": "emb",
            "query_text": "harbour",
            "k": 5,
        });
        let decode = |extra: serde_json::Value| {
            let mut body = base.clone();
            body.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            serde_json::from_value::<types::HybridSearchReq>(body)
        };

        assert_eq!(decode(serde_json::json!({})).unwrap().ef, None);
        assert_eq!(decode(serde_json::json!({ "ef": null })).unwrap().ef, None);
        assert_eq!(
            decode(serde_json::json!({ "ef": 256 })).unwrap().ef,
            Some(256)
        );
        assert!(decode(serde_json::json!({ "ef": -1 })).is_err());
        assert!(decode(serde_json::json!({ "ef": "256" })).is_err());
        assert!(decode(serde_json::json!({ "ef": 1.5 })).is_err());
        assert!(decode(serde_json::json!({ "ef": 4_294_967_296_u64 })).is_err());
    }
}

/// The beam width of a hybrid search's vector half (grafeo-server#85), on
/// the engine's fixture for the same feature: a vector index built with a
/// narrow construction beam, so the default search beam misses exact
/// neighbours and `ef` changes what the vector half finds.
#[cfg(all(test, feature = "hybrid-search", feature = "vector-index"))]
mod hybrid_ef_tests {
    use std::collections::HashMap;
    use std::sync::OnceLock;

    use grafeo_common::Value;

    use super::*;
    use crate::ServiceState;

    const DIMENSIONS: usize = 16;
    const NODES: usize = 3000;
    /// The results asked for; each half fetches twice as many.
    const K: u32 = 5;
    /// A text that no document contains, so the vector half alone decides.
    const NO_TEXT_MATCH: &str = "zzzz";
    /// Query seeds; the default beam misses an exact neighbour for most.
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

    struct Fixture {
        state: ServiceState,
        vectors: Vec<(u64, Vec<f32>)>,
    }

    fn fixture() -> &'static Fixture {
        static FIXTURE: OnceLock<Fixture> = OnceLock::new();
        FIXTURE.get_or_init(|| {
            let state = ServiceState::new_in_memory(300);
            let db = state.databases().get_available("default").unwrap().db();
            let mut values = Lcg(0x5EED);
            let mut vectors = Vec::with_capacity(NODES);
            for i in 0..NODES {
                let id = db.create_node(&["Doc"]).unwrap();
                let text = if i % 3 == 0 {
                    "Lisbon harbour at dawn"
                } else {
                    "Porto bridges at night"
                };
                db.set_node_property(id, "content", Value::String(text.into()))
                    .unwrap();
                let vector = values.vector();
                db.set_node_property(id, "emb", Value::Vector(vector.clone().into()))
                    .unwrap();
                vectors.push((id.as_u64(), vector));
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
            Fixture { state, vectors }
        })
    }

    fn query_vector(seed: u64) -> Vec<f32> {
        Lcg(seed).vector()
    }

    fn hybrid_req(
        query_text: &str,
        query_vector: Vec<f32>,
        ef: Option<u32>,
    ) -> types::HybridSearchReq {
        types::HybridSearchReq {
            database: "default".into(),
            label: "Doc".into(),
            text_property: "content".into(),
            vector_property: "emb".into(),
            query_text: query_text.into(),
            query_vector,
            k: K,
            ef,
        }
    }

    async fn hybrid(
        query_text: &str,
        query_vector: Vec<f32>,
        ef: Option<u32>,
    ) -> Vec<types::SearchHit> {
        SearchService::hybrid_search(
            fixture().state.databases(),
            "default",
            hybrid_req(query_text, query_vector, ef),
        )
        .await
        .unwrap()
    }

    fn ids(hits: &[types::SearchHit]) -> Vec<u64> {
        hits.iter().map(|h| h.node_id).collect()
    }

    /// The `k` nodes nearest to `query` by a scan of every vector.
    fn nearest_by_scan(query: &[f32], k: usize) -> Vec<u64> {
        let mut by_distance: Vec<(f32, u64)> = fixture()
            .vectors
            .iter()
            .map(|(id, v)| {
                let d: f32 = v.iter().zip(query).map(|(a, b)| (a - b) * (a - b)).sum();
                (d, *id)
            })
            .collect();
        by_distance.sort_by(|a, b| a.0.total_cmp(&b.0));
        by_distance.into_iter().take(k).map(|(_, id)| id).collect()
    }

    fn sorted(mut ids: Vec<u64>) -> Vec<u64> {
        ids.sort_unstable();
        ids
    }

    #[tokio::test]
    async fn a_full_beam_finds_the_exact_nearest_neighbours() {
        let full = u32::try_from(NODES).unwrap();
        let mut default_missed = 0;
        for seed in SEEDS {
            let query = query_vector(seed);
            let exact = sorted(nearest_by_scan(&query, K as usize));
            let at_full_beam = sorted(ids(&hybrid(NO_TEXT_MATCH, query.clone(), Some(full)).await));
            assert_eq!(at_full_beam, exact, "seed {seed}: ef={full} must be exact");
            let at_default = sorted(ids(&hybrid(NO_TEXT_MATCH, query, None).await));
            if at_default != exact {
                default_missed += 1;
            }
        }
        // Without this the test could not tell whether `ef` reached the engine.
        assert!(
            default_missed > 0,
            "the default beam found every exact neighbour; the fixture no longer shows ef"
        );
    }

    #[tokio::test]
    async fn the_vector_half_matches_vector_search_at_the_same_ef() {
        for ef in [None, Some(8), Some(64), Some(400)] {
            for seed in SEEDS {
                let query = query_vector(seed);
                let hybrid_ids = ids(&hybrid(NO_TEXT_MATCH, query.clone(), ef).await);
                let vector = SearchService::vector_search(
                    fixture().state.databases(),
                    "default",
                    types::VectorSearchReq {
                        database: "default".into(),
                        label: "Doc".into(),
                        property: "emb".into(),
                        query_vector: query,
                        k: 2 * K,
                        ef,
                        filters: HashMap::new(),
                    },
                )
                .await
                .unwrap();
                // With an empty text half, RRF keeps the vector half's order.
                let expected: Vec<u64> = ids(&vector).into_iter().take(K as usize).collect();
                assert_eq!(hybrid_ids, expected, "seed {seed}, ef {ef:?}");
            }
        }
    }

    #[tokio::test]
    async fn without_ef_the_results_are_the_engines_default_hybrid_search() {
        let db = fixture()
            .state
            .databases()
            .get_available("default")
            .unwrap()
            .db();
        for seed in SEEDS {
            let query = query_vector(seed);
            let expected: Vec<(u64, f64)> = db
                .hybrid_search(
                    "Doc",
                    "content",
                    "emb",
                    "harbour",
                    Some(&query),
                    K as usize,
                    None,
                    None,
                )
                .unwrap()
                .into_iter()
                .map(|(id, score)| (id.as_u64(), score))
                .collect();
            let got: Vec<(u64, f64)> = hybrid("harbour", query, None)
                .await
                .into_iter()
                .map(|h| (h.node_id, h.score))
                .collect();
            assert_eq!(got, expected, "seed {seed}");
        }
    }

    #[tokio::test]
    async fn ef_without_a_query_vector_is_accepted_and_changes_nothing() {
        let text_only = hybrid("harbour", Vec::new(), None).await;
        assert_eq!(text_only.len(), K as usize);
        for ef in [Some(1), Some(64), Some(u32::MAX)] {
            let with_ef = hybrid("harbour", Vec::new(), ef).await;
            assert_eq!(ids(&with_ef), ids(&text_only), "ef {ef:?}");
        }
    }

    #[tokio::test]
    async fn a_small_or_huge_beam_still_returns_k_results() {
        for ef in [1, 2, u32::MAX] {
            let hits = hybrid(NO_TEXT_MATCH, query_vector(1), Some(ef)).await;
            assert_eq!(hits.len(), K as usize, "ef {ef}");
        }
    }
}
