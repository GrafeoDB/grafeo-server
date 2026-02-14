//! Transaction management endpoints.
//!
//! Session lifecycle is delegated to `grafeo_service::query::QueryService`.

use axum::extract::{Json, State};
use axum::http::HeaderMap;

use grafeo_service::query::QueryService;

use crate::error::{ApiError, ErrorBody};
use crate::state::AppState;

use super::helpers::{convert_json_params, query_result_to_response};
use super::types::{QueryRequest, QueryResponse, TransactionResponse, TxBeginRequest};

fn get_session_id(headers: &HeaderMap) -> Result<String, ApiError> {
    headers
        .get("x-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .ok_or_else(|| ApiError::bad_request("missing X-Session-Id header"))
}

/// Begin a new transaction.
///
/// Returns a session ID to use with subsequent `/tx/query`, `/tx/commit`,
/// and `/tx/rollback` requests via the `X-Session-Id` header.
/// Optionally specify `database` to target a specific database.
#[utoipa::path(
    post,
    path = "/tx/begin",
    request_body(content = Option<TxBeginRequest>, description = "Optional database selection"),
    responses(
        (status = 200, description = "Transaction started", body = TransactionResponse),
        (status = 404, description = "Database not found", body = ErrorBody),
        (status = 500, description = "Internal server error", body = ErrorBody),
    ),
    tag = "Transaction"
)]
pub async fn tx_begin(
    State(state): State<AppState>,
    body: Option<Json<TxBeginRequest>>,
) -> Result<Json<TransactionResponse>, ApiError> {
    let db_name = body
        .as_ref()
        .and_then(|b| b.database.as_deref())
        .unwrap_or("default");

    let session_id = QueryService::begin_tx(state.databases(), state.sessions(), db_name).await?;

    Ok(Json(TransactionResponse {
        session_id,
        status: "open".to_string(),
    }))
}

/// Execute a query within a transaction.
///
/// Requires an `X-Session-Id` header from a prior `/tx/begin` call.
#[utoipa::path(
    post,
    path = "/tx/query",
    request_body = QueryRequest,
    params(
        ("x-session-id" = String, Header, description = "Transaction session ID from /tx/begin"),
    ),
    responses(
        (status = 200, description = "Query executed successfully", body = QueryResponse),
        (status = 400, description = "Bad request or missing session header", body = ErrorBody),
        (status = 404, description = "Session not found or expired", body = ErrorBody),
    ),
    tag = "Transaction"
)]
pub async fn tx_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, ApiError> {
    let session_id = get_session_id(&headers)?;
    let params = convert_json_params(req.params.as_ref())?;
    let timeout = state.effective_timeout(req.timeout_ms);

    let result = QueryService::tx_execute(
        state.sessions(),
        state.metrics(),
        &session_id,
        state.session_ttl(),
        &req.query,
        req.language.as_deref(),
        params,
        timeout,
    )
    .await?;

    Ok(Json(query_result_to_response(&result)))
}

/// Commit a transaction.
///
/// Persists all changes made within the transaction and removes the session.
/// Requires an `X-Session-Id` header.
#[utoipa::path(
    post,
    path = "/tx/commit",
    params(
        ("x-session-id" = String, Header, description = "Transaction session ID from /tx/begin"),
    ),
    responses(
        (status = 200, description = "Transaction committed", body = TransactionResponse),
        (status = 400, description = "Missing session header", body = ErrorBody),
        (status = 404, description = "Session not found or expired", body = ErrorBody),
    ),
    tag = "Transaction"
)]
pub async fn tx_commit(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<TransactionResponse>, ApiError> {
    let session_id = get_session_id(&headers)?;

    QueryService::commit(state.sessions(), &session_id, state.session_ttl()).await?;

    Ok(Json(TransactionResponse {
        session_id,
        status: "committed".to_string(),
    }))
}

/// Roll back a transaction.
///
/// Discards all changes made within the transaction and removes the session.
/// Requires an `X-Session-Id` header.
#[utoipa::path(
    post,
    path = "/tx/rollback",
    params(
        ("x-session-id" = String, Header, description = "Transaction session ID from /tx/begin"),
    ),
    responses(
        (status = 200, description = "Transaction rolled back", body = TransactionResponse),
        (status = 400, description = "Missing session header", body = ErrorBody),
        (status = 404, description = "Session not found or expired", body = ErrorBody),
    ),
    tag = "Transaction"
)]
pub async fn tx_rollback(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<TransactionResponse>, ApiError> {
    let session_id = get_session_id(&headers)?;

    QueryService::rollback(state.sessions(), &session_id, state.session_ttl()).await?;

    Ok(Json(TransactionResponse {
        session_id,
        status: "rolled_back".to_string(),
    }))
}
