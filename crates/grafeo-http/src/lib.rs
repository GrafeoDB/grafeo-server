//! Grafeo HTTP — REST API transport adapter for Grafeo Server.
//!
//! Provides the HTTP/REST interface including:
//! - Query endpoints (GQL, Cypher, GraphQL, Gremlin, SPARQL, SQL/PGQ)
//! - Transaction management (begin/query/commit/rollback)
//! - Batch queries
//! - WebSocket endpoint
//! - Database management (create/delete/list/info)
//! - System/health endpoints
//! - OpenAPI/Swagger UI
//! - Rate limiting, auth, request-ID middleware

pub mod encode;
pub mod error;
pub mod middleware;
pub mod routes;
pub mod state;
#[cfg(feature = "tls")]
pub mod tls;
pub mod types;

use axum::Router;
use axum::http::{HeaderValue, Method};
use axum::routing::{delete, get, post};
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use error::ErrorBody;
use types::{DatabaseSummary, SearchResponse};

pub use state::AppState;

// ---------------------------------------------------------------------------
// OpenAPI
// ---------------------------------------------------------------------------

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Grafeo Server API",
        description = "HTTP API for the Grafeo graph database engine.\n\nSupports GQL, Cypher, GraphQL, Gremlin, SPARQL, and SQL/PGQ query languages with both auto-commit and explicit transaction modes.\n\nAll query languages support CALL procedures for 22+ built-in graph algorithms (PageRank, BFS, WCC, Dijkstra, Louvain, etc.).\n\nMulti-database support: create, delete, and query named databases.",
        version = "0.4.2",
        license(name = "Apache-2.0"),
    ),
    paths(
        routes::system::health,
        routes::system::system_resources,
        routes::query::query,
        routes::query::cypher,
        routes::query::graphql,
        routes::query::gremlin,
        routes::query::sparql,
        routes::query::sql,
        routes::batch::batch_query,
        routes::transaction::tx_begin,
        routes::transaction::tx_query,
        routes::transaction::tx_commit,
        routes::transaction::tx_rollback,
        routes::database::list_databases,
        routes::database::create_database,
        routes::database::delete_database,
        routes::database::database_info,
        routes::database::database_stats,
        routes::database::database_schema,
        routes::admin::admin_stats,
        routes::admin::admin_wal_status,
        routes::admin::admin_wal_checkpoint,
        routes::admin::admin_validate,
        routes::admin::admin_create_index,
        routes::admin::admin_drop_index,
        routes::search::vector_search,
        routes::search::text_search,
        routes::search::hybrid_search,
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
            grafeo_service::types::DatabaseStats, grafeo_service::types::WalStatusInfo,
            grafeo_service::types::ValidationInfo, grafeo_service::types::ValidationErrorItem,
            grafeo_service::types::ValidationWarningItem, grafeo_service::types::IndexDef,
            grafeo_service::types::VectorSearchReq, grafeo_service::types::TextSearchReq,
            grafeo_service::types::HybridSearchReq, grafeo_service::types::SearchHit,
            SearchResponse,
        )
    ),
    tags(
        (name = "Query", description = "Execute queries in various graph query languages"),
        (name = "Transaction", description = "Explicit transaction management"),
        (name = "Database", description = "Database management (create, delete, list, info)"),
        (name = "Admin", description = "Database administration, introspection, and index management"),
        (name = "Search", description = "Vector, text, and hybrid search"),
        (name = "System", description = "System and health endpoints"),
    )
)]
struct ApiDoc;

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Builds the HTTP API router.
///
/// Call this from the binary crate to get a fully-wired axum `Router`.
/// Optionally merge Studio UI routes before serving.
pub fn router(state: AppState) -> Router {
    let api = Router::new()
        // Query endpoints
        .route("/query", post(routes::query::query))
        .route("/cypher", post(routes::query::cypher))
        .route("/graphql", post(routes::query::graphql))
        .route("/gremlin", post(routes::query::gremlin))
        .route("/sparql", post(routes::query::sparql))
        .route("/sql", post(routes::query::sql))
        .route("/batch", post(routes::batch::batch_query))
        // WebSocket
        .route("/ws", get(routes::websocket::ws_handler))
        // Transaction endpoints
        .route("/tx/begin", post(routes::transaction::tx_begin))
        .route("/tx/query", post(routes::transaction::tx_query))
        .route("/tx/commit", post(routes::transaction::tx_commit))
        .route("/tx/rollback", post(routes::transaction::tx_rollback))
        // Database management
        .route(
            "/db",
            get(routes::database::list_databases).post(routes::database::create_database),
        )
        .route(
            "/db/{name}",
            delete(routes::database::delete_database).get(routes::database::database_info),
        )
        .route("/db/{name}/stats", get(routes::database::database_stats))
        .route("/db/{name}/schema", get(routes::database::database_schema))
        // Admin
        .route("/admin/{db}/stats", get(routes::admin::admin_stats))
        .route("/admin/{db}/wal", get(routes::admin::admin_wal_status))
        .route(
            "/admin/{db}/wal/checkpoint",
            post(routes::admin::admin_wal_checkpoint),
        )
        .route("/admin/{db}/validate", get(routes::admin::admin_validate))
        .route(
            "/admin/{db}/index",
            post(routes::admin::admin_create_index).delete(routes::admin::admin_drop_index),
        )
        // Search
        .route("/search/vector", post(routes::search::vector_search))
        .route("/search/text", post(routes::search::text_search))
        .route("/search/hybrid", post(routes::search::hybrid_search))
        // System
        .route("/health", get(routes::system::health))
        .route("/system/resources", get(routes::system::system_resources))
        .route("/metrics", get(routes::system::metrics_endpoint))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http());

    #[cfg(feature = "auth")]
    let api = api.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        middleware::auth::auth_middleware,
    ));

    let api = api
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::rate_limit::rate_limit_middleware,
        ))
        .layer(axum::middleware::from_fn(
            middleware::request_id::request_id_middleware,
        ))
        .layer(cors_layer(&state))
        .with_state(state);

    api.merge(SwaggerUi::new("/api/docs").url("/api/openapi.json", ApiDoc::openapi()))
}

/// Serve the HTTP router on the given listener with graceful shutdown.
///
/// Wraps `axum::serve` with `ConnectInfo<SocketAddr>` so rate limiting
/// and logging can extract client addresses.
pub async fn serve(
    listener: tokio::net::TcpListener,
    app: Router,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) {
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .expect("server error");
}

fn cors_layer(state: &AppState) -> CorsLayer {
    let origins = state.cors_origins();

    // No origins configured → no CORS headers (deny cross-origin by default).
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
