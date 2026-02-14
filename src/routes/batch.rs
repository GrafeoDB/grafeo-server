//! Batch query endpoint — execute multiple queries in a single transaction.
//!
//! Delegates to `grafeo_service::query::QueryService::batch_execute`.

use axum::extract::{Json, State};

use grafeo_service::query::QueryService;
use grafeo_service::types::BatchQuery;

use crate::error::{ApiError, ErrorBody};
use crate::state::AppState;

use super::helpers::{convert_json_params, query_result_to_response};
use super::types::{BatchQueryRequest, BatchQueryResponse};

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

    let db_name = grafeo_service::resolve_db_name(req.database.as_deref());
    let timeout = state.effective_timeout(req.timeout_ms);

    // Convert HTTP batch items to service BatchQuery (JSON params → engine params)
    let batch_queries: Vec<BatchQuery> = req
        .queries
        .iter()
        .map(|item| {
            let params = convert_json_params(item.params.as_ref())?;
            Ok(BatchQuery {
                statement: item.query.clone(),
                language: item.language.clone(),
                params,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;

    let results =
        QueryService::batch_execute(state.databases(), state.metrics(), db_name, batch_queries, timeout)
            .await?;

    let responses: Vec<_> = results.iter().map(query_result_to_response).collect();
    let total_ms: f64 = results
        .iter()
        .filter_map(|r| r.execution_time_ms)
        .sum();

    Ok(Json(BatchQueryResponse {
        results: responses,
        total_execution_time_ms: total_ms,
    }))
}
