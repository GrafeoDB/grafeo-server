//! HTTP API routes for Grafeo Server.

mod batch;
mod database;
mod helpers;
mod query;
mod system;
mod transaction;
pub mod types;
mod websocket;

use axum::Router;
use axum::http::{HeaderValue, Method};
use axum::middleware;
use axum::routing::{delete, get, post};
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::error::ErrorBody;
use crate::rate_limit::rate_limit_middleware;
use crate::request_id::request_id_middleware;
use crate::state::AppState;

use types::DatabaseSummary;

// ---------------------------------------------------------------------------
// OpenAPI
// ---------------------------------------------------------------------------

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Grafeo Server API",
        description = "HTTP API for the Grafeo graph database engine.\n\nSupports GQL, Cypher, GraphQL, Gremlin, SPARQL, and SQL/PGQ query languages with both auto-commit and explicit transaction modes.\n\nAll query languages support CALL procedures for 22+ built-in graph algorithms (PageRank, BFS, WCC, Dijkstra, Louvain, etc.).\n\nMulti-database support: create, delete, and query named databases.",
        version = "0.4.0",
        license(name = "Apache-2.0"),
    ),
    paths(
        system::health,
        system::system_resources,
        query::query,
        query::cypher,
        query::graphql,
        query::gremlin,
        query::sparql,
        query::sql,
        batch::batch_query,
        transaction::tx_begin,
        transaction::tx_query,
        transaction::tx_commit,
        transaction::tx_rollback,
        database::list_databases,
        database::create_database,
        database::delete_database,
        database::database_info,
        database::database_stats,
        database::database_schema,
    ),
    components(
        schemas(
            types::QueryRequest, types::QueryResponse, types::TxBeginRequest,
            types::TransactionResponse, types::HealthResponse, types::EnabledFeatures, ErrorBody,
            types::CreateDatabaseRequest, types::DatabaseType, types::StorageMode,
            types::DatabaseOptions, types::ListDatabasesResponse, DatabaseSummary,
            types::DatabaseInfoResponse, types::DatabaseStatsResponse,
            types::DatabaseSchemaResponse, types::LabelInfo, types::EdgeTypeInfo,
            types::SystemResources, types::ResourceDefaults,
            types::BatchQueryRequest, types::BatchQueryItem, types::BatchQueryResponse,
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
        .route("/query", post(query::query))
        .route("/cypher", post(query::cypher))
        .route("/graphql", post(query::graphql))
        .route("/gremlin", post(query::gremlin))
        .route("/sparql", post(query::sparql))
        .route("/sql", post(query::sql))
        .route("/batch", post(batch::batch_query))
        // WebSocket
        .route("/ws", get(websocket::ws_handler))
        // Transaction endpoints
        .route("/tx/begin", post(transaction::tx_begin))
        .route("/tx/query", post(transaction::tx_query))
        .route("/tx/commit", post(transaction::tx_commit))
        .route("/tx/rollback", post(transaction::tx_rollback))
        // Database management
        .route(
            "/db",
            get(database::list_databases).post(database::create_database),
        )
        .route(
            "/db/{name}",
            delete(database::delete_database).get(database::database_info),
        )
        .route("/db/{name}/stats", get(database::database_stats))
        .route("/db/{name}/schema", get(database::database_schema))
        // System
        .route("/health", get(system::health))
        .route("/system/resources", get(system::system_resources))
        .route("/metrics", get(system::metrics_endpoint))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http());

    #[cfg(feature = "auth")]
    let api = api.layer(middleware::from_fn_with_state(
        state.clone(),
        crate::auth::auth_middleware,
    ));

    let api = api
        .layer(middleware::from_fn_with_state(
            state.clone(),
            rate_limit_middleware,
        ))
        .layer(middleware::from_fn(request_id_middleware))
        .layer(cors_layer(&state))
        .with_state(state.clone());

    // Studio UI: embedded React app via rust-embed
    #[cfg(feature = "studio")]
    let app = crate::ui::router().merge(api);
    #[cfg(not(feature = "studio"))]
    let app = api;

    app.merge(SwaggerUi::new("/api/docs").url("/api/openapi.json", ApiDoc::openapi()))
}

fn cors_layer(state: &AppState) -> CorsLayer {
    let origins = state.cors_origins();

    // No origins configured → no CORS headers (deny cross-origin by default).
    // Use --cors-origins "*" for permissive or specify exact origins.
    if origins.is_empty() {
        return CorsLayer::new();
    }

    let x_request_id = axum::http::header::HeaderName::from_static("x-request-id");
    let base = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
            axum::http::header::HeaderName::from_static("x-session-id"),
            axum::http::header::HeaderName::from_static("x-api-key"),
            x_request_id.clone(),
        ])
        .expose_headers([x_request_id]);

    if origins.len() == 1 && origins[0] == "*" {
        tracing::warn!("CORS configured with wildcard origin — all cross-origin requests allowed");
        base.allow_origin(tower_http::cors::Any)
    } else {
        let parsed: Vec<HeaderValue> = origins
            .iter()
            .map(|o| o.parse().expect("invalid CORS origin"))
            .collect();
        base.allow_origin(parsed)
    }
}
