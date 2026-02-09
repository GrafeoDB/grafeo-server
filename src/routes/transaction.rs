//! Transaction management endpoints.

use std::sync::Arc;

use axum::extract::{Json, State};
use axum::http::HeaderMap;

use crate::database_manager::DatabaseEntry;
use crate::error::{ApiError, ErrorBody};
use crate::metrics::determine_language;
use crate::state::AppState;

use super::helpers::{effective_timeout, record_metrics, run_with_timeout};
use super::query::run_query;
use super::types::{QueryRequest, QueryResponse, TransactionResponse, TxBeginRequest};

fn get_session_id(headers: &HeaderMap) -> Result<String, ApiError> {
    headers
        .get("x-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .ok_or(ApiError::BadRequest(
            "missing X-Session-Id header".to_string(),
        ))
}

/// Resolves the database entry for an existing transaction session.
fn resolve_tx_db(
    state: &AppState,
    session_id: &str,
) -> Result<(String, Arc<DatabaseEntry>), ApiError> {
    let db_name = state
        .databases()
        .resolve_session(session_id)
        .ok_or(ApiError::SessionNotFound)?;
    let entry = state
        .databases()
        .get(&db_name)
        .ok_or(ApiError::SessionNotFound)?;
    Ok((db_name, entry))
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
        .unwrap_or("default")
        .to_string();

    let entry = state
        .databases()
        .get(&db_name)
        .ok_or_else(|| ApiError::NotFound(format!("database '{db_name}' not found")))?;

    let databases = state.databases();
    let session_id = tokio::task::spawn_blocking(move || {
        let mut engine_session = entry.db.session();
        engine_session
            .begin_tx()
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        Ok::<_, ApiError>(entry.sessions.create(engine_session))
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))??;

    databases.register_session(&session_id, &db_name);

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
    let ttl = state.session_ttl();
    let (_, entry) = resolve_tx_db(&state, &session_id)?;

    let session_arc = entry
        .sessions
        .get(&session_id, ttl)
        .ok_or(ApiError::SessionNotFound)?;

    let timeout = effective_timeout(&state, req.timeout_ms);
    let lang = determine_language(req.language.as_deref());

    let result = run_with_timeout(timeout, move || {
        let session = session_arc.lock();
        run_query(&session.engine_session, &req)
    })
    .await;

    record_metrics(&state, lang, &result);
    Ok(Json(result?))
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
    let ttl = state.session_ttl();
    let (_, entry) = resolve_tx_db(&state, &session_id)?;

    let session_arc = entry
        .sessions
        .get(&session_id, ttl)
        .ok_or(ApiError::SessionNotFound)?;

    tokio::task::spawn_blocking(move || {
        let mut session = session_arc.lock();
        session
            .engine_session
            .commit()
            .map_err(|e| ApiError::Internal(e.to_string()))
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))??;

    entry.sessions.remove(&session_id);
    state.databases().unregister_session(&session_id);

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
    let ttl = state.session_ttl();
    let (_, entry) = resolve_tx_db(&state, &session_id)?;

    let session_arc = entry
        .sessions
        .get(&session_id, ttl)
        .ok_or(ApiError::SessionNotFound)?;

    tokio::task::spawn_blocking(move || {
        let mut session = session_arc.lock();
        session
            .engine_session
            .rollback()
            .map_err(|e| ApiError::Internal(e.to_string()))
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))??;

    entry.sessions.remove(&session_id);
    state.databases().unregister_session(&session_id);

    Ok(Json(TransactionResponse {
        session_id,
        status: "rolled_back".to_string(),
    }))
}
