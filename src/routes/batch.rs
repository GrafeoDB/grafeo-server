//! Batch query endpoint — execute multiple queries in a single transaction.

use axum::extract::{Json, State};

use crate::error::{ApiError, ErrorBody};
use crate::state::AppState;

use super::helpers::{effective_timeout, resolve_db_name, run_with_timeout};
use super::query::run_query;
use super::types::{
    BatchQueryItem, BatchQueryRequest, BatchQueryResponse, QueryRequest, QueryResponse,
};

/// Execute a batch of queries in a single transaction.
///
/// All queries run sequentially within one transaction. If any query fails,
/// the transaction is rolled back and an error is returned indicating which
/// query failed. On success, all changes are committed atomically.
#[utoipa::path(
    post,
    path = "/batch",
    request_body = BatchQueryRequest,
    responses(
        (status = 200, description = "All queries executed successfully", body = BatchQueryResponse),
        (status = 400, description = "Query failed", body = ErrorBody),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Query"
)]
pub async fn batch_query(
    State(state): State<AppState>,
    Json(req): Json<BatchQueryRequest>,
) -> Result<Json<BatchQueryResponse>, ApiError> {
    if req.queries.is_empty() {
        return Ok(Json(BatchQueryResponse {
            results: vec![],
            total_execution_time_ms: 0.0,
        }));
    }

    let db_name = resolve_db_name(req.database.as_ref()).to_string();
    let entry = state
        .databases()
        .get(&db_name)
        .ok_or_else(|| ApiError::NotFound(format!("database '{db_name}' not found")))?;

    let timeout = effective_timeout(&state, req.timeout_ms);

    let result = run_with_timeout(timeout, move || {
        let mut session = entry.db.session();
        session
            .begin_tx()
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        let mut results: Vec<QueryResponse> = Vec::with_capacity(req.queries.len());

        for (idx, item) in req.queries.iter().enumerate() {
            let query_req = item_to_query_request(item);
            match run_query(&session, &query_req) {
                Ok(resp) => results.push(resp),
                Err(e) => {
                    let _ = session.rollback();
                    return Err(ApiError::BadRequest(format!(
                        "query at index {idx} failed: {e}"
                    )));
                }
            }
        }

        session
            .commit()
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        let total_ms: f64 = results.iter().filter_map(|r| r.execution_time_ms).sum();

        Ok(BatchQueryResponse {
            results,
            total_execution_time_ms: total_ms,
        })
    })
    .await?;

    Ok(Json(result))
}

/// Converts a `BatchQueryItem` into a `QueryRequest` for `run_query`.
fn item_to_query_request(item: &BatchQueryItem) -> QueryRequest {
    QueryRequest {
        query: item.query.clone(),
        language: item.language.clone(),
        params: item.params.clone(),
        database: None,
        timeout_ms: None,
    }
}
