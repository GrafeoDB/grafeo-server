//! Query execution endpoints.

use std::collections::HashMap;

use axum::extract::{Json, State};

use crate::error::{ApiError, ErrorBody};
use crate::metrics::{Language, determine_language};
use crate::state::AppState;

use super::helpers::{effective_timeout, record_metrics, resolve_db_name, run_with_timeout};
use super::types::{QueryRequest, QueryResponse};

/// Converts a Grafeo `Value` to a JSON value.
fn value_to_json(value: &grafeo_common::Value) -> serde_json::Value {
    use grafeo_common::Value;
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int64(i) => serde_json::json!(i),
        Value::Float64(f) => serde_json::json!(f),
        Value::String(s) => serde_json::Value::String(s.to_string()),
        Value::Bytes(b) => serde_json::json!(b.as_ref()),
        Value::Timestamp(t) => serde_json::Value::String(format!("{t:?}")),
        Value::List(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
        Value::Map(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.to_string(), value_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        Value::Vector(v) => serde_json::json!(v.as_ref()),
    }
}

/// Executes a query on the given engine session, dispatching by language.
pub fn run_query(
    session: &grafeo_engine::Session,
    req: &QueryRequest,
) -> Result<QueryResponse, ApiError> {
    let result = if let Some(ref params_json) = req.params {
        let params: HashMap<String, grafeo_common::Value> =
            serde_json::from_value(params_json.clone())
                .map_err(|e| ApiError::BadRequest(format!("invalid params: {e}")))?;
        match req.language.as_deref() {
            Some("cypher") => session.execute_cypher(&req.query),
            Some("graphql") => session.execute_graphql_with_params(&req.query, params),
            Some("gremlin") => session.execute_gremlin_with_params(&req.query, params),
            Some("sparql") => session.execute_sparql(&req.query),
            _ => session.execute_with_params(&req.query, params),
        }
    } else {
        match req.language.as_deref() {
            Some("cypher") => session.execute_cypher(&req.query),
            Some("graphql") => session.execute_graphql(&req.query),
            Some("gremlin") => session.execute_gremlin(&req.query),
            Some("sparql") => session.execute_sparql(&req.query),
            _ => session.execute(&req.query),
        }
    }
    .map_err(|e| ApiError::BadRequest(e.to_string()))?;

    Ok(QueryResponse {
        columns: result.columns.clone(),
        rows: result
            .rows
            .iter()
            .map(|row| row.iter().map(value_to_json).collect())
            .collect(),
        execution_time_ms: result.execution_time_ms,
        rows_scanned: result.rows_scanned,
    })
}

/// Execute a query (auto-commit).
///
/// Runs a query in the specified language (defaults to GQL).
/// Each request uses a fresh session that auto-commits on success.
/// Optionally specify `database` to target a specific database (defaults to "default").
#[utoipa::path(
    post,
    path = "/query",
    request_body = QueryRequest,
    responses(
        (status = 200, description = "Query executed successfully", body = QueryResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Query"
)]
pub async fn query(
    State(state): State<AppState>,
    Json(req): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, ApiError> {
    let db_name = resolve_db_name(req.database.as_ref()).to_string();
    let entry = state
        .databases()
        .get(&db_name)
        .ok_or_else(|| ApiError::NotFound(format!("database '{db_name}' not found")))?;

    let timeout = effective_timeout(&state, req.timeout_ms);
    let lang = determine_language(req.language.as_deref());

    let result = run_with_timeout(timeout, move || {
        let session = entry.db.session();
        run_query(&session, &req)
    })
    .await;

    record_metrics(&state, lang, &result);
    Ok(Json(result?))
}

/// Execute a Cypher query (auto-commit).
///
/// Convenience endpoint — the `language` field is ignored; queries
/// are always interpreted as Cypher.
#[utoipa::path(
    post,
    path = "/cypher",
    request_body = QueryRequest,
    responses(
        (status = 200, description = "Query executed successfully", body = QueryResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Query"
)]
pub async fn cypher(
    State(state): State<AppState>,
    Json(mut req): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, ApiError> {
    req.language = Some("cypher".to_string());
    let db_name = resolve_db_name(req.database.as_ref()).to_string();
    let entry = state
        .databases()
        .get(&db_name)
        .ok_or_else(|| ApiError::NotFound(format!("database '{db_name}' not found")))?;

    let timeout = effective_timeout(&state, req.timeout_ms);
    let result = run_with_timeout(timeout, move || {
        let session = entry.db.session();
        run_query(&session, &req)
    })
    .await;

    record_metrics(&state, Language::Cypher, &result);
    Ok(Json(result?))
}

/// Execute a GraphQL query (auto-commit).
///
/// Convenience endpoint — the `language` field is ignored; queries
/// are always interpreted as GraphQL.
#[utoipa::path(
    post,
    path = "/graphql",
    request_body = QueryRequest,
    responses(
        (status = 200, description = "Query executed successfully", body = QueryResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Query"
)]
pub async fn graphql(
    State(state): State<AppState>,
    Json(mut req): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, ApiError> {
    req.language = Some("graphql".to_string());
    let db_name = resolve_db_name(req.database.as_ref()).to_string();
    let entry = state
        .databases()
        .get(&db_name)
        .ok_or_else(|| ApiError::NotFound(format!("database '{db_name}' not found")))?;

    let timeout = effective_timeout(&state, req.timeout_ms);
    let result = run_with_timeout(timeout, move || {
        let session = entry.db.session();
        run_query(&session, &req)
    })
    .await;

    record_metrics(&state, Language::Graphql, &result);
    Ok(Json(result?))
}

/// Execute a Gremlin query (auto-commit).
///
/// Convenience endpoint — the `language` field is ignored; queries
/// are always interpreted as Gremlin.
#[utoipa::path(
    post,
    path = "/gremlin",
    request_body = QueryRequest,
    responses(
        (status = 200, description = "Query executed successfully", body = QueryResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Query"
)]
pub async fn gremlin(
    State(state): State<AppState>,
    Json(mut req): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, ApiError> {
    req.language = Some("gremlin".to_string());
    let db_name = resolve_db_name(req.database.as_ref()).to_string();
    let entry = state
        .databases()
        .get(&db_name)
        .ok_or_else(|| ApiError::NotFound(format!("database '{db_name}' not found")))?;

    let timeout = effective_timeout(&state, req.timeout_ms);
    let result = run_with_timeout(timeout, move || {
        let session = entry.db.session();
        run_query(&session, &req)
    })
    .await;

    record_metrics(&state, Language::Gremlin, &result);
    Ok(Json(result?))
}

/// Execute a SPARQL query (auto-commit).
///
/// Convenience endpoint — the `language` field is ignored; queries
/// are always interpreted as SPARQL.
#[utoipa::path(
    post,
    path = "/sparql",
    request_body = QueryRequest,
    responses(
        (status = 200, description = "Query executed successfully", body = QueryResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Query"
)]
pub async fn sparql(
    State(state): State<AppState>,
    Json(mut req): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, ApiError> {
    req.language = Some("sparql".to_string());
    let db_name = resolve_db_name(req.database.as_ref()).to_string();
    let entry = state
        .databases()
        .get(&db_name)
        .ok_or_else(|| ApiError::NotFound(format!("database '{db_name}' not found")))?;

    let timeout = effective_timeout(&state, req.timeout_ms);
    let result = run_with_timeout(timeout, move || {
        let session = entry.db.session();
        run_query(&session, &req)
    })
    .await;

    record_metrics(&state, Language::Sparql, &result);
    Ok(Json(result?))
}
