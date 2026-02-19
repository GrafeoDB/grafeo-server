//! Search endpoints — vector, text, and hybrid search.

use axum::extract::{Json, State};

use crate::error::ApiError;
use crate::state::AppState;
use crate::types::SearchResponse;

use grafeo_service::search::SearchService;
use grafeo_service::types;

/// Vector similarity search (KNN via HNSW index).
///
/// Requires a vector index on the target label/property.
#[utoipa::path(
    post,
    path = "/search/vector",
    request_body = types::VectorSearchReq,
    responses(
        (status = 200, description = "Search results", body = SearchResponse),
        (status = 400, description = "Invalid request or feature disabled", body = crate::error::ErrorBody),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Search"
)]
pub async fn vector_search(
    State(state): State<AppState>,
    Json(req): Json<types::VectorSearchReq>,
) -> Result<Json<SearchResponse>, ApiError> {
    let db_name = req.database.clone();
    let hits = SearchService::vector_search(state.databases(), &db_name, req).await?;
    Ok(Json(SearchResponse { hits }))
}

/// Full-text search (BM25 scoring).
///
/// Requires a text index on the target label/property.
#[utoipa::path(
    post,
    path = "/search/text",
    request_body = types::TextSearchReq,
    responses(
        (status = 200, description = "Search results", body = SearchResponse),
        (status = 400, description = "Invalid request or feature disabled", body = crate::error::ErrorBody),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Search"
)]
pub async fn text_search(
    State(state): State<AppState>,
    Json(req): Json<types::TextSearchReq>,
) -> Result<Json<SearchResponse>, ApiError> {
    let db_name = req.database.clone();
    let hits = SearchService::text_search(state.databases(), &db_name, req).await?;
    Ok(Json(SearchResponse { hits }))
}

/// Hybrid search (vector + text with rank fusion).
///
/// Combines BM25 text scoring with vector similarity for better recall.
#[utoipa::path(
    post,
    path = "/search/hybrid",
    request_body = types::HybridSearchReq,
    responses(
        (status = 200, description = "Search results", body = SearchResponse),
        (status = 400, description = "Invalid request or feature disabled", body = crate::error::ErrorBody),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Search"
)]
pub async fn hybrid_search(
    State(state): State<AppState>,
    Json(req): Json<types::HybridSearchReq>,
) -> Result<Json<SearchResponse>, ApiError> {
    let db_name = req.database.clone();
    let hits = SearchService::hybrid_search(state.databases(), &db_name, req).await?;
    Ok(Json(SearchResponse { hits }))
}
