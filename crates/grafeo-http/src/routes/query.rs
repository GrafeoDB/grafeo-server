//! Query execution endpoints.
//!
//! All language dispatch, timeout handling, and metrics recording is
//! delegated to `grafeo_service::query::QueryService`.

use axum::extract::{Json, State};
use axum::http::HeaderMap;
use axum::response::Response;
use grafeo_engine::database::QueryResult;

use grafeo_service::query::QueryService;

use crate::encode::{convert_json_params, streaming_json_response};
use crate::error::{ApiError, ErrorBody};
use crate::middleware::auth_context::AuthContext;
use crate::state::AppState;
use crate::types::{QueryRequest, QueryResponse};

/// Check if the client accepts Arrow IPC format.
#[cfg(feature = "arrow-export")]
fn accepts_arrow(headers: &HeaderMap) -> bool {
    headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .map(|a| a.contains("application/vnd.apache.arrow.stream"))
        .unwrap_or(false)
}

/// Serialize a query result as Arrow IPC.
#[cfg(feature = "arrow-export")]
fn arrow_ipc_response(result: QueryResult) -> Result<Response, ApiError> {
    let ipc_bytes = result
        .to_arrow_ipc()
        .map_err(|e| ApiError::internal(format!("Arrow export failed: {e}")))?;
    Ok(axum::response::Response::builder()
        .header("Content-Type", "application/vnd.apache.arrow.stream")
        .body(axum::body::Body::from(ipc_bytes))
        .expect("valid response"))
}

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
    headers: HeaderMap,
    Json(req): Json<QueryRequest>,
) -> Result<Response, ApiError> {
    let result = execute_query(&state, &auth, &req, None).await?;
    #[cfg(feature = "arrow-export")]
    if accepts_arrow(&headers) {
        return arrow_ipc_response(result);
    }
    let _ = &headers;
    Ok(streaming_json_response(result))
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
    headers: HeaderMap,
    Json(req): Json<QueryRequest>,
) -> Result<Response, ApiError> {
    let result = execute_query(&state, &auth, &req, Some("cypher")).await?;
    #[cfg(feature = "arrow-export")]
    if accepts_arrow(&headers) {
        return arrow_ipc_response(result);
    }
    let _ = &headers;
    Ok(streaming_json_response(result))
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
    headers: HeaderMap,
    Json(req): Json<QueryRequest>,
) -> Result<Response, ApiError> {
    let result = execute_query(&state, &auth, &req, Some("graphql")).await?;
    #[cfg(feature = "arrow-export")]
    if accepts_arrow(&headers) {
        return arrow_ipc_response(result);
    }
    let _ = &headers;
    Ok(streaming_json_response(result))
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
    headers: HeaderMap,
    Json(req): Json<QueryRequest>,
) -> Result<Response, ApiError> {
    let result = execute_query(&state, &auth, &req, Some("gremlin")).await?;
    #[cfg(feature = "arrow-export")]
    if accepts_arrow(&headers) {
        return arrow_ipc_response(result);
    }
    let _ = &headers;
    Ok(streaming_json_response(result))
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
    headers: HeaderMap,
    Json(req): Json<QueryRequest>,
) -> Result<Response, ApiError> {
    let result = execute_query(&state, &auth, &req, Some("sparql")).await?;
    #[cfg(feature = "arrow-export")]
    if accepts_arrow(&headers) {
        return arrow_ipc_response(result);
    }
    let _ = &headers;
    Ok(streaming_json_response(result))
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
    headers: HeaderMap,
    Json(req): Json<QueryRequest>,
) -> Result<Response, ApiError> {
    let result = execute_query(&state, &auth, &req, Some("sql-pgq")).await?;
    #[cfg(feature = "arrow-export")]
    if accepts_arrow(&headers) {
        return arrow_ipc_response(result);
    }
    let _ = &headers;
    Ok(streaming_json_response(result))
}
