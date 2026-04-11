//! Query execution endpoints.
//!
//! All language dispatch, timeout handling, and metrics recording is
//! delegated to `grafeo_service::query::QueryService`.

use axum::extract::{Json, State};
use axum::response::Response;
use grafeo_engine::database::QueryResult;

use grafeo_service::query::QueryService;

use crate::encode::{convert_json_params, streaming_json_response};
use crate::error::{ApiError, ErrorBody};
use crate::middleware::auth_context::AuthContext;
use crate::state::AppState;
use crate::types::{QueryRequest, QueryResponse};

/// Shared implementation for all auto-commit query endpoints.
async fn execute_query(
    state: &AppState,
    auth: &AuthContext,
    req: &QueryRequest,
    lang_override: Option<&str>,
) -> Result<QueryResult, ApiError> {
    let language = lang_override.or(req.language.as_deref());
    let db_name = grafeo_service::resolve_db_name(req.database.as_deref());
    auth.check_db_access(db_name)?;
    let params = convert_json_params(req.params.as_ref())?;
    let timeout = state.effective_timeout(req.timeout_ms);

    let identity = auth.identity(state.service().is_query_read_only());

    let result = QueryService::execute(
        state.databases(),
        state.metrics(),
        db_name,
        &req.query,
        language,
        params,
        timeout,
        state.service().is_query_read_only(),
        Some(identity),
    )
    .await?;

    Ok(result)
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
    auth: AuthContext,
    Json(req): Json<QueryRequest>,
) -> Result<Response, ApiError> {
    Ok(streaming_json_response(
        execute_query(&state, &auth, &req, None).await?,
    ))
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
    auth: AuthContext,
    Json(req): Json<QueryRequest>,
) -> Result<Response, ApiError> {
    Ok(streaming_json_response(
        execute_query(&state, &auth, &req, Some("cypher")).await?,
    ))
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
    auth: AuthContext,
    Json(req): Json<QueryRequest>,
) -> Result<Response, ApiError> {
    Ok(streaming_json_response(
        execute_query(&state, &auth, &req, Some("graphql")).await?,
    ))
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
    auth: AuthContext,
    Json(req): Json<QueryRequest>,
) -> Result<Response, ApiError> {
    Ok(streaming_json_response(
        execute_query(&state, &auth, &req, Some("gremlin")).await?,
    ))
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
    auth: AuthContext,
    Json(req): Json<QueryRequest>,
) -> Result<Response, ApiError> {
    Ok(streaming_json_response(
        execute_query(&state, &auth, &req, Some("sparql")).await?,
    ))
}

/// Execute a SQL/PGQ query (auto-commit).
///
/// SQL/PGQ (Property Graph Queries) extends SQL with graph pattern matching.
/// Also supports CALL procedure syntax for graph algorithms.
#[utoipa::path(
    post,
    path = "/sql",
    request_body = QueryRequest,
    responses(
        (status = 200, description = "Query executed successfully", body = QueryResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Query"
)]
pub async fn sql(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(req): Json<QueryRequest>,
) -> Result<Response, ApiError> {
    Ok(streaming_json_response(
        execute_query(&state, &auth, &req, Some("sql-pgq")).await?,
    ))
}
