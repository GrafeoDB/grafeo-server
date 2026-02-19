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
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        let results = tokio::task::spawn_blocking(move || {
            let filters = if req.filters.is_empty() {
                None
            } else {
                Some(req.filters)
            };
            entry.db.vector_search(
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
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        let results = tokio::task::spawn_blocking(move || {
            entry
                .db
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
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        let results = tokio::task::spawn_blocking(move || {
            let query_vec = if req.query_vector.is_empty() {
                None
            } else {
                Some(req.query_vector)
            };
            entry.db.hybrid_search(
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
