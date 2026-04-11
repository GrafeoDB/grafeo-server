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
                node_id: node_id.0,
                score: f64::from(distance),
                properties: std::collections::HashMap::new(),
            })
            .collect())
    }

    /// Vector search stub when feature is disabled.
    #[cfg(not(feature = "vector-index"))]
    #[allow(clippy::unused_async)]
    pub async fn vector_search(
        _databases: &DatabaseManager,
        _db_name: &str,
        _req: types::VectorSearchReq,
    ) -> Result<Vec<types::SearchHit>, ServiceError> {
        Err(ServiceError::BadRequest(
            "vector-index feature not enabled".to_owned(),
        ))
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
                .text_search(&req.label, &req.property, &req.query, req.k as usize)
        })
        .await
        .map_err(|e| ServiceError::Internal(e.to_string()))?
        .map_err(|e| ServiceError::BadRequest(e.to_string()))?;

        Ok(results
            .into_iter()
            .map(|(node_id, score)| types::SearchHit {
                node_id: node_id.0,
                score,
                properties: std::collections::HashMap::new(),
            })
            .collect())
    }

    /// Text search stub when feature is disabled.
    #[cfg(not(feature = "text-index"))]
    #[allow(clippy::unused_async)]
    pub async fn text_search(
        _databases: &DatabaseManager,
        _db_name: &str,
        _req: types::TextSearchReq,
    ) -> Result<Vec<types::SearchHit>, ServiceError> {
        Err(ServiceError::BadRequest(
            "text-index feature not enabled".to_owned(),
        ))
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
            let query_vec = if req.query_vector.is_empty() {
                None
            } else {
                Some(req.query_vector)
            };
            entry.db().hybrid_search(
                &req.label,
                &req.text_property,
                &req.vector_property,
                &req.query_text,
                query_vec.as_deref(),
                req.k as usize,
                None,
            )
        })
        .await
        .map_err(|e| ServiceError::Internal(e.to_string()))?
        .map_err(|e| ServiceError::BadRequest(e.to_string()))?;

        Ok(results
            .into_iter()
            .map(|(node_id, score)| types::SearchHit {
                node_id: node_id.0,
                score,
                properties: std::collections::HashMap::new(),
            })
            .collect())
    }

    /// Hybrid search stub when feature is disabled.
    #[cfg(not(feature = "hybrid-search"))]
    #[allow(clippy::unused_async)]
    pub async fn hybrid_search(
        _databases: &DatabaseManager,
        _db_name: &str,
        _req: types::HybridSearchReq,
    ) -> Result<Vec<types::SearchHit>, ServiceError> {
        Err(ServiceError::BadRequest(
            "hybrid-search feature not enabled".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
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
            filters: Default::default(),
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
            filters: Default::default(),
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
        };
        let err = SearchService::hybrid_search(s.databases(), "default", req)
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::BadRequest(_)));
        assert!(err.to_string().contains("hybrid-search"));
    }
}
