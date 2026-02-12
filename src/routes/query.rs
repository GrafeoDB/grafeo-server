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

/// Returns an error for a query language that is not compiled into this build.
#[allow(dead_code)]
fn language_not_enabled(lang: &str) -> ApiError {
    ApiError::BadRequest(format!(
        "language '{lang}' is not enabled in this server build"
    ))
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
            #[cfg(feature = "cypher")]
            Some("cypher") => session.execute_cypher(&req.query),
            #[cfg(not(feature = "cypher"))]
            Some("cypher") => return Err(language_not_enabled("cypher")),
            #[cfg(feature = "graphql")]
            Some("graphql") => session.execute_graphql_with_params(&req.query, params),
            #[cfg(not(feature = "graphql"))]
            Some("graphql") => return Err(language_not_enabled("graphql")),
            #[cfg(feature = "gremlin")]
            Some("gremlin") => session.execute_gremlin_with_params(&req.query, params),
            #[cfg(not(feature = "gremlin"))]
            Some("gremlin") => return Err(language_not_enabled("gremlin")),
            #[cfg(feature = "sparql")]
            Some("sparql") => session.execute_sparql(&req.query),
            #[cfg(not(feature = "sparql"))]
            Some("sparql") => return Err(language_not_enabled("sparql")),
            _ => session.execute_with_params(&req.query, params),
        }
    } else {
        match req.language.as_deref() {
            #[cfg(feature = "cypher")]
            Some("cypher") => session.execute_cypher(&req.query),
            #[cfg(not(feature = "cypher"))]
            Some("cypher") => return Err(language_not_enabled("cypher")),
            #[cfg(feature = "graphql")]
            Some("graphql") => session.execute_graphql(&req.query),
            #[cfg(not(feature = "graphql"))]
            Some("graphql") => return Err(language_not_enabled("graphql")),
            #[cfg(feature = "gremlin")]
            Some("gremlin") => session.execute_gremlin(&req.query),
            #[cfg(not(feature = "gremlin"))]
            Some("gremlin") => return Err(language_not_enabled("gremlin")),
            #[cfg(feature = "sparql")]
            Some("sparql") => session.execute_sparql(&req.query),
            #[cfg(not(feature = "sparql"))]
            Some("sparql") => return Err(language_not_enabled("sparql")),
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

/// Shared implementation for all auto-commit query endpoints.
async fn execute_auto_commit(
    state: &AppState,
    mut req: QueryRequest,
    lang_override: Option<(&str, Language)>,
) -> Result<Json<QueryResponse>, ApiError> {
    if let Some((name, _)) = lang_override {
        req.language = Some(name.to_string());
    }
    let db_name = resolve_db_name(req.database.as_ref()).to_string();
    let entry = state
        .databases()
        .get(&db_name)
        .ok_or_else(|| ApiError::NotFound(format!("database '{db_name}' not found")))?;

    let timeout = effective_timeout(state, req.timeout_ms);
    let lang =
        lang_override.map_or_else(|| determine_language(req.language.as_deref()), |(_, l)| l);

    let result = run_with_timeout(timeout, move || {
        let session = entry.db.session();
        run_query(&session, &req)
    })
    .await;

    record_metrics(state, lang, &result);
    Ok(Json(result?))
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
    execute_auto_commit(&state, req, None).await
}

/// Execute a Cypher query (auto-commit).
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
    Json(req): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, ApiError> {
    execute_auto_commit(&state, req, Some(("cypher", Language::Cypher))).await
}

/// Execute a GraphQL query (auto-commit).
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
    Json(req): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, ApiError> {
    execute_auto_commit(&state, req, Some(("graphql", Language::Graphql))).await
}

/// Execute a Gremlin query (auto-commit).
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
    Json(req): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, ApiError> {
    execute_auto_commit(&state, req, Some(("gremlin", Language::Gremlin))).await
}

/// Execute a SPARQL query (auto-commit).
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
    Json(req): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, ApiError> {
    execute_auto_commit(&state, req, Some(("sparql", Language::Sparql))).await
}
