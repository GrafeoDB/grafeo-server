//! HTTP API routes for Grafeo Server.

use std::collections::HashMap;
use std::time::Duration;

use axum::Router;
use axum::extract::{Json, Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use serde::{Deserialize, Serialize};
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;
use utoipa::ToSchema;
use utoipa_swagger_ui::SwaggerUi;

use crate::auth::auth_middleware;
use crate::database_manager::DatabaseSummary;
use crate::error::{ApiError, ErrorBody};
use crate::metrics::{Language, determine_language};
use crate::request_id::request_id_middleware;
use crate::state::AppState;
use crate::ui;

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

#[derive(Deserialize, ToSchema)]
pub struct QueryRequest {
    /// The query string to execute.
    query: String,
    /// Optional query parameters (JSON object).
    #[serde(default)]
    params: Option<serde_json::Value>,
    /// Query language: "gql" (default), "cypher", "graphql", "gremlin", "sparql".
    /// Ignored by language-specific convenience endpoints.
    #[serde(default)]
    language: Option<String>,
    /// Target database name (defaults to "default").
    #[serde(default)]
    database: Option<String>,
    /// Per-query timeout override in milliseconds (0 = use server default).
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Serialize, ToSchema)]
pub struct QueryResponse {
    /// Column names from the result set.
    columns: Vec<String>,
    /// Result rows, each containing JSON-encoded values.
    rows: Vec<Vec<serde_json::Value>>,
    /// Time taken to execute the query in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_time_ms: Option<f64>,
    /// Number of rows scanned during query execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    rows_scanned: Option<u64>,
}

#[derive(Deserialize, ToSchema)]
pub struct TxBeginRequest {
    /// Target database name (defaults to "default").
    #[serde(default)]
    database: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct TransactionResponse {
    /// Unique session identifier for the transaction.
    session_id: String,
    /// Transaction status: "open", "committed", or "rolled_back".
    status: String,
}

#[derive(Serialize, ToSchema)]
pub struct HealthResponse {
    /// Server status ("ok").
    status: String,
    /// Server version.
    version: String,
    /// Grafeo engine version.
    engine_version: String,
    /// Whether the server is using persistent storage.
    persistent: bool,
    /// Server uptime in seconds.
    uptime_seconds: u64,
    /// Number of active transaction sessions across all databases.
    active_sessions: usize,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateDatabaseRequest {
    /// Name for the new database.
    name: String,
}

#[derive(Serialize, ToSchema)]
pub struct ListDatabasesResponse {
    /// List of all databases.
    databases: Vec<DatabaseSummary>,
}

#[derive(Serialize, ToSchema)]
pub struct DatabaseInfoResponse {
    /// Database name.
    name: String,
    /// Number of nodes.
    node_count: usize,
    /// Number of edges.
    edge_count: usize,
    /// Whether the database uses persistent storage.
    persistent: bool,
    /// Database version string from the engine.
    version: String,
    /// Whether WAL is enabled.
    wal_enabled: bool,
}

#[derive(Serialize, ToSchema)]
pub struct DatabaseStatsResponse {
    /// Database name.
    name: String,
    /// Number of nodes.
    node_count: usize,
    /// Number of edges.
    edge_count: usize,
    /// Number of distinct labels.
    label_count: usize,
    /// Number of distinct edge types.
    edge_type_count: usize,
    /// Number of distinct property keys.
    property_key_count: usize,
    /// Number of indexes.
    index_count: usize,
    /// Approximate memory usage in bytes.
    memory_bytes: usize,
    /// Approximate disk usage in bytes (persistent only).
    #[serde(skip_serializing_if = "Option::is_none")]
    disk_bytes: Option<usize>,
}

#[derive(Serialize, ToSchema)]
pub struct DatabaseSchemaResponse {
    /// Database name.
    name: String,
    /// Node labels with counts.
    labels: Vec<LabelInfo>,
    /// Edge types with counts.
    edge_types: Vec<EdgeTypeInfo>,
    /// Property key names.
    property_keys: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct LabelInfo {
    /// Label name.
    name: String,
    /// Number of nodes with this label.
    count: usize,
}

#[derive(Serialize, ToSchema)]
pub struct EdgeTypeInfo {
    /// Edge type name.
    name: String,
    /// Number of edges with this type.
    count: usize,
}

// ---------------------------------------------------------------------------
// OpenAPI
// ---------------------------------------------------------------------------

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Grafeo Server API",
        description = "HTTP API for the Grafeo graph database engine.\n\nSupports GQL, Cypher, GraphQL, Gremlin, and SPARQL query languages with both auto-commit and explicit transaction modes.\n\nMulti-database support: create, delete, and query named databases.",
        version = "0.2.0",
        license(name = "AGPL-3.0-or-later"),
    ),
    paths(
        health,
        query,
        cypher,
        graphql,
        gremlin,
        sparql,
        tx_begin,
        tx_query,
        tx_commit,
        tx_rollback,
        list_databases,
        create_database,
        delete_database,
        database_info,
        database_stats,
        database_schema,
    ),
    components(
        schemas(
            QueryRequest, QueryResponse, TxBeginRequest,
            TransactionResponse, HealthResponse, ErrorBody,
            CreateDatabaseRequest, ListDatabasesResponse, DatabaseSummary,
            DatabaseInfoResponse, DatabaseStatsResponse, DatabaseSchemaResponse,
            LabelInfo, EdgeTypeInfo,
        )
    ),
    tags(
        (name = "Query", description = "Execute queries in various graph query languages"),
        (name = "Transaction", description = "Explicit transaction management"),
        (name = "Database", description = "Database management (create, delete, list, info)"),
        (name = "System", description = "System and health endpoints"),
    )
)]
pub struct ApiDoc;

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Builds the main application router.
pub fn router(state: AppState) -> Router {
    let api = Router::new()
        // Query endpoints
        .route("/query", post(query))
        .route("/cypher", post(cypher))
        .route("/graphql", post(graphql))
        .route("/gremlin", post(gremlin))
        .route("/sparql", post(sparql))
        // Transaction endpoints
        .route("/tx/begin", post(tx_begin))
        .route("/tx/query", post(tx_query))
        .route("/tx/commit", post(tx_commit))
        .route("/tx/rollback", post(tx_rollback))
        // Database management
        .route("/db", get(list_databases).post(create_database))
        .route("/db/{name}", delete(delete_database).get(database_info))
        .route("/db/{name}/stats", get(database_stats))
        .route("/db/{name}/schema", get(database_schema))
        // System
        .route("/health", get(health))
        .route("/metrics", get(metrics_endpoint))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .layer(middleware::from_fn(request_id_middleware))
        .layer(cors_layer(&state))
        .with_state(state.clone());

    // Mount the web UI, API, and Swagger docs
    ui::router()
        .merge(api)
        .merge(SwaggerUi::new("/api/docs").url("/api/openapi.json", ApiDoc::openapi()))
}

fn cors_layer(state: &AppState) -> CorsLayer {
    let x_request_id = axum::http::header::HeaderName::from_static("x-request-id");
    let base = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
            axum::http::header::HeaderName::from_static("x-session-id"),
            x_request_id.clone(),
        ])
        .expose_headers([x_request_id]);

    let origins = state.cors_origins();
    if origins.is_empty() {
        base.allow_origin(tower_http::cors::Any)
    } else {
        let parsed: Vec<HeaderValue> = origins
            .iter()
            .map(|o| o.parse().expect("invalid CORS origin"))
            .collect();
        base.allow_origin(parsed)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolves the database name from the request, defaulting to "default".
fn resolve_db_name(database: Option<&String>) -> &str {
    database.map_or("default", |s| s.as_str())
}

/// Computes the effective timeout for a query.
fn effective_timeout(state: &AppState, req_timeout_ms: Option<u64>) -> Option<Duration> {
    match req_timeout_ms {
        Some(0) => None,
        Some(ms) => Some(Duration::from_millis(ms)),
        None => {
            let global = state.query_timeout();
            if global.is_zero() { None } else { Some(global) }
        }
    }
}

/// Runs a blocking task with an optional timeout.
async fn run_with_timeout<F, T>(timeout: Option<Duration>, task: F) -> Result<T, ApiError>
where
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
    T: Send + 'static,
{
    let handle = tokio::task::spawn_blocking(task);
    if let Some(dur) = timeout {
        match tokio::time::timeout(dur, handle).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => Err(ApiError::Internal(e.to_string())),
            Err(_) => {
                tracing::warn!("query timed out after {dur:?}");
                Err(ApiError::Timeout)
            }
        }
    } else {
        handle
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
    }
}

/// Records query metrics after execution.
fn record_metrics(state: &AppState, lang: Language, result: &Result<QueryResponse, ApiError>) {
    match result {
        Ok(resp) => {
            let dur_us = resp.execution_time_ms.map_or(0, |ms| (ms * 1000.0) as u64);
            state.metrics().record_query(lang, dur_us);
        }
        Err(_) => {
            state.metrics().record_query_error(lang);
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Check server health.
///
/// Returns server status, version info, and whether persistent storage is enabled.
#[utoipa::path(
    get,
    path = "/health",
    responses(
        (status = 200, description = "Server is healthy", body = HealthResponse),
    ),
    tag = "System"
)]
async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let dbs = state.databases();
    let persistent = dbs.data_dir().is_some();
    let active_sessions: usize = dbs
        .list()
        .iter()
        .filter_map(|s| dbs.get(&s.name))
        .map(|e| e.sessions.active_count())
        .sum();

    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        engine_version: "0.4.0".to_string(),
        persistent,
        uptime_seconds: state.uptime_secs(),
        active_sessions,
    })
}

/// Prometheus-compatible metrics endpoint.
async fn metrics_endpoint(State(state): State<AppState>) -> impl IntoResponse {
    let dbs = state.databases();
    let db_list = dbs.list();
    let databases_total = db_list.len();
    let nodes_total: usize = db_list.iter().map(|d| d.node_count).sum();
    let edges_total: usize = db_list.iter().map(|d| d.edge_count).sum();
    let active_sessions: usize = db_list
        .iter()
        .filter_map(|s| dbs.get(&s.name))
        .map(|e| e.sessions.active_count())
        .sum();

    let body = state.metrics().render(
        databases_total,
        nodes_total,
        edges_total,
        active_sessions,
        state.uptime_secs(),
    );

    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

/// Executes a query on the given engine session, dispatching by language.
fn run_query(
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
async fn query(
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
async fn cypher(
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
async fn graphql(
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
async fn gremlin(
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
async fn sparql(
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

// ---------------------------------------------------------------------------
// Transactions
// ---------------------------------------------------------------------------

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
async fn tx_begin(
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
) -> Result<
    (
        String,
        std::sync::Arc<crate::database_manager::DatabaseEntry>,
    ),
    ApiError,
> {
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
async fn tx_query(
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
async fn tx_commit(
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
async fn tx_rollback(
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

// ---------------------------------------------------------------------------
// Database management
// ---------------------------------------------------------------------------

/// List all databases.
///
/// Returns summary information for each database including node/edge counts.
#[utoipa::path(
    get,
    path = "/db",
    responses(
        (status = 200, description = "List of databases", body = ListDatabasesResponse),
    ),
    tag = "Database"
)]
async fn list_databases(State(state): State<AppState>) -> impl IntoResponse {
    let databases = state.databases().list();
    Json(ListDatabasesResponse { databases })
}

/// Create a new database.
///
/// Creates a named database. Name must start with a letter, contain only
/// alphanumeric characters, underscores, or hyphens, and be at most 64 characters.
#[utoipa::path(
    post,
    path = "/db",
    request_body = CreateDatabaseRequest,
    responses(
        (status = 200, description = "Database created", body = DatabaseSummary),
        (status = 400, description = "Invalid database name", body = ErrorBody),
        (status = 409, description = "Database already exists", body = ErrorBody),
    ),
    tag = "Database"
)]
async fn create_database(
    State(state): State<AppState>,
    Json(req): Json<CreateDatabaseRequest>,
) -> Result<Json<DatabaseSummary>, ApiError> {
    let name = req.name.clone();
    state.databases().create(&name)?;

    let entry = state
        .databases()
        .get(&name)
        .ok_or_else(|| ApiError::Internal("database disappeared after creation".to_string()))?;

    Ok(Json(DatabaseSummary {
        name,
        node_count: entry.db.node_count(),
        edge_count: entry.db.edge_count(),
        persistent: entry.db.path().is_some(),
    }))
}

/// Delete a database.
///
/// Removes a named database and all its data. The "default" database cannot be deleted.
#[utoipa::path(
    delete,
    path = "/db/{name}",
    params(
        ("name" = String, Path, description = "Database name to delete"),
    ),
    responses(
        (status = 200, description = "Database deleted"),
        (status = 400, description = "Cannot delete default database", body = ErrorBody),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Database"
)]
async fn delete_database(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    state.databases().delete(&name)?;
    Ok(Json(serde_json::json!({ "deleted": name })))
}

/// Get database info.
///
/// Returns metadata about a specific database.
#[utoipa::path(
    get,
    path = "/db/{name}",
    params(
        ("name" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "Database info", body = DatabaseInfoResponse),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Database"
)]
async fn database_info(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<DatabaseInfoResponse>, ApiError> {
    let entry = state
        .databases()
        .get(&name)
        .ok_or_else(|| ApiError::NotFound(format!("database '{name}' not found")))?;

    let info = entry.db.info();
    Ok(Json(DatabaseInfoResponse {
        name,
        node_count: info.node_count,
        edge_count: info.edge_count,
        persistent: info.is_persistent,
        version: info.version,
        wal_enabled: info.wal_enabled,
    }))
}

/// Get database statistics.
///
/// Returns detailed statistics including memory and disk usage.
#[utoipa::path(
    get,
    path = "/db/{name}/stats",
    params(
        ("name" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "Database statistics", body = DatabaseStatsResponse),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Database"
)]
async fn database_stats(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<DatabaseStatsResponse>, ApiError> {
    let entry = state
        .databases()
        .get(&name)
        .ok_or_else(|| ApiError::NotFound(format!("database '{name}' not found")))?;

    let stats = entry.db.detailed_stats();
    Ok(Json(DatabaseStatsResponse {
        name,
        node_count: stats.node_count,
        edge_count: stats.edge_count,
        label_count: stats.label_count,
        edge_type_count: stats.edge_type_count,
        property_key_count: stats.property_key_count,
        index_count: stats.index_count,
        memory_bytes: stats.memory_bytes,
        disk_bytes: stats.disk_bytes,
    }))
}

/// Get database schema.
///
/// Returns labels, edge types, and property keys for a database.
#[utoipa::path(
    get,
    path = "/db/{name}/schema",
    params(
        ("name" = String, Path, description = "Database name"),
    ),
    responses(
        (status = 200, description = "Database schema", body = DatabaseSchemaResponse),
        (status = 404, description = "Database not found", body = ErrorBody),
    ),
    tag = "Database"
)]
async fn database_schema(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<DatabaseSchemaResponse>, ApiError> {
    let entry = state
        .databases()
        .get(&name)
        .ok_or_else(|| ApiError::NotFound(format!("database '{name}' not found")))?;

    let schema = entry.db.schema();
    // We only support LPG mode for now
    match schema {
        grafeo_engine::admin::SchemaInfo::Lpg(lpg) => Ok(Json(DatabaseSchemaResponse {
            name,
            labels: lpg
                .labels
                .into_iter()
                .map(|l| LabelInfo {
                    name: l.name,
                    count: l.count,
                })
                .collect(),
            edge_types: lpg
                .edge_types
                .into_iter()
                .map(|e| EdgeTypeInfo {
                    name: e.name,
                    count: e.count,
                })
                .collect(),
            property_keys: lpg.property_keys,
        })),
        grafeo_engine::admin::SchemaInfo::Rdf(_) => Err(ApiError::BadRequest(
            "RDF schema not supported via this endpoint".to_string(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Value conversion
// ---------------------------------------------------------------------------

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
