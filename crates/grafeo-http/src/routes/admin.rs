//! Admin endpoints — database introspection, maintenance, and index management.

use axum::extract::{Json, Path, State};
use axum::response::IntoResponse;

use crate::error::ApiError;
use crate::middleware::auth_context::AuthContext;
use crate::state::AppState;

use grafeo_service::admin::AdminService;
use grafeo_service::types;

/// Get detailed database statistics.
#[utoipa::path(
    get, path = "/admin/{db}/stats",
    params(("db" = String, Path, description = "Database name")),
    responses(
        (status = 200, description = "Database statistics", body = types::DatabaseStats),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_stats(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
) -> Result<Json<types::DatabaseStats>, ApiError> {
    auth.check_admin()?;
    let stats = AdminService::database_stats(state.databases(), &db).await?;
    Ok(Json(stats))
}

/// Get WAL status for a database.
#[utoipa::path(
    get, path = "/admin/{db}/wal",
    params(("db" = String, Path, description = "Database name")),
    responses(
        (status = 200, description = "WAL status", body = types::WalStatusInfo),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_wal_status(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
) -> Result<Json<types::WalStatusInfo>, ApiError> {
    auth.check_admin()?;
    let status = AdminService::wal_status(state.databases(), &db).await?;
    Ok(Json(status))
}

/// Force a WAL checkpoint.
#[utoipa::path(
    post, path = "/admin/{db}/wal/checkpoint",
    params(("db" = String, Path, description = "Database name")),
    responses(
        (status = 200, description = "Checkpoint completed"),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_wal_checkpoint(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    auth.check_admin()?;
    AdminService::wal_checkpoint(state.databases(), &db).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// Validate database integrity.
#[utoipa::path(
    get, path = "/admin/{db}/validate",
    params(("db" = String, Path, description = "Database name")),
    responses(
        (status = 200, description = "Validation result", body = types::ValidationInfo),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_validate(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
) -> Result<Json<types::ValidationInfo>, ApiError> {
    auth.check_admin()?;
    let result = AdminService::validate(state.databases(), &db).await?;
    Ok(Json(result))
}

/// Create an index on a database.
#[utoipa::path(
    post, path = "/admin/{db}/index",
    params(("db" = String, Path, description = "Database name")),
    request_body = types::IndexDef,
    responses(
        (status = 200, description = "Index created"),
        (status = 400, description = "Invalid index definition", body = crate::error::ErrorBody),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_create_index(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
    Json(index): Json<types::IndexDef>,
) -> Result<impl IntoResponse, ApiError> {
    auth.check_admin()?;
    AdminService::create_index(state.databases(), &db, index).await?;
    Ok(Json(serde_json::json!({ "created": true })))
}

/// Get query plan cache statistics.
#[utoipa::path(
    get, path = "/admin/{db}/cache",
    params(("db" = String, Path, description = "Database name")),
    responses(
        (status = 200, description = "Cache statistics", body = types::CacheStatsInfo),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_cache_stats(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
) -> Result<Json<types::CacheStatsInfo>, ApiError> {
    auth.check_admin()?;
    let stats = AdminService::cache_stats(state.databases(), &db).await?;
    Ok(Json(stats))
}

/// Clear the query plan cache.
#[utoipa::path(
    post, path = "/admin/{db}/cache/clear",
    params(("db" = String, Path, description = "Database name")),
    responses(
        (status = 200, description = "Cache cleared"),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_clear_cache(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    auth.check_admin()?;
    AdminService::clear_cache(state.databases(), &db).await?;
    Ok(Json(serde_json::json!({ "cleared": true })))
}

/// Get hierarchical memory usage breakdown.
#[utoipa::path(
    get, path = "/admin/{db}/memory",
    params(("db" = String, Path, description = "Database name")),
    responses(
        (status = 200, description = "Memory usage breakdown"),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_memory_usage(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    auth.check_admin()?;
    let usage = AdminService::memory_usage(state.databases(), &db).await?;
    Ok(Json(usage))
}

/// Drop an index from a database.
#[utoipa::path(
    delete, path = "/admin/{db}/index",
    params(("db" = String, Path, description = "Database name")),
    request_body = types::IndexDef,
    responses(
        (status = 200, description = "Index drop result"),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_drop_index(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
    Json(index): Json<types::IndexDef>,
) -> Result<impl IntoResponse, ApiError> {
    auth.check_admin()?;
    let dropped = AdminService::drop_index(state.databases(), &db, index).await?;
    Ok(Json(serde_json::json!({ "dropped": dropped })))
}

/// Compact a database into a columnar read-only store.
#[utoipa::path(
    post, path = "/admin/{db}/compact",
    params(("db" = String, Path, description = "Database name")),
    responses(
        (status = 200, description = "Database compacted"),
        (status = 400, description = "Feature not enabled", body = crate::error::ErrorBody),
        (status = 403, description = "Server is read-only", body = crate::error::ErrorBody),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
        (status = 409, description = "Database in use", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_compact(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    auth.check_admin()?;
    AdminService::compact(state.databases(), &db).await?;
    Ok(Json(serde_json::json!({ "compacted": true })))
}

/// Write a point-in-time snapshot.
#[utoipa::path(
    post, path = "/admin/{db}/snapshot",
    params(("db" = String, Path, description = "Database name")),
    responses(
        (status = 200, description = "Snapshot created"),
        (status = 400, description = "Feature not enabled", body = crate::error::ErrorBody),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_write_snapshot(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    auth.check_admin()?;
    AdminService::write_snapshot(state.databases(), &db).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// Create a graph projection.
#[utoipa::path(
    post, path = "/admin/{db}/projections",
    params(("db" = String, Path, description = "Database name")),
    request_body = types::CreateProjectionRequest,
    responses(
        (status = 200, description = "Projection created (true) or already exists (false)", body = bool),
        (status = 403, description = "Read-only mode", body = crate::error::ErrorBody),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_create_projection(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
    Json(req): Json<types::CreateProjectionRequest>,
) -> Result<Json<bool>, ApiError> {
    auth.check_admin()?;
    let created = AdminService::create_projection(state.databases(), &db, req).await?;
    Ok(Json(created))
}

/// List graph projections.
#[utoipa::path(
    get, path = "/admin/{db}/projections",
    params(("db" = String, Path, description = "Database name")),
    responses(
        (status = 200, description = "Projection list", body = types::ProjectionListResponse),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_list_projections(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
) -> Result<Json<types::ProjectionListResponse>, ApiError> {
    auth.check_admin()?;
    let projections = AdminService::list_projections(state.databases(), &db).await?;
    Ok(Json(types::ProjectionListResponse { projections }))
}

/// Drop a graph projection.
#[utoipa::path(
    delete, path = "/admin/{db}/projections/{name}",
    params(
        ("db" = String, Path, description = "Database name"),
        ("name" = String, Path, description = "Projection name"),
    ),
    responses(
        (status = 200, description = "Projection dropped (true) or not found (false)", body = bool),
        (status = 403, description = "Read-only mode", body = crate::error::ErrorBody),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_drop_projection(
    State(state): State<AppState>,
    auth: AuthContext,
    Path((db, name)): Path<(String, String)>,
) -> Result<Json<bool>, ApiError> {
    auth.check_admin()?;
    let dropped = AdminService::drop_projection(state.databases(), &db, &name).await?;
    Ok(Json(dropped))
}

/// Validate RDF data against SHACL shapes.
#[utoipa::path(
    post, path = "/admin/{db}/validate/shacl",
    params(("db" = String, Path, description = "Database name")),
    request_body = types::ShaclValidateRequest,
    responses(
        (status = 200, description = "Validation report", body = types::ShaclValidationReport),
        (status = 400, description = "Feature not enabled or invalid shapes", body = crate::error::ErrorBody),
        (status = 404, description = "Database not found", body = crate::error::ErrorBody),
    ),
    tag = "Admin"
)]
pub async fn admin_validate_shacl(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(db): Path<String>,
    Json(req): Json<types::ShaclValidateRequest>,
) -> Result<Json<types::ShaclValidationReport>, ApiError> {
    auth.check_admin()?;
    let report = AdminService::validate_shacl(state.databases(), &db, &req).await?;
    Ok(Json(report))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn app() -> axum::Router {
        let state = crate::AppState::new_in_memory(300);
        crate::router(state)
    }

    // -----------------------------------------------------------------------
    // Projection endpoints
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn create_projection_returns_ok() {
        let resp = app()
            .oneshot(
                Request::post("/admin/default/projections")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"name":"test","node_labels":["A"],"edge_types":["R"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            resp.status() == StatusCode::OK || resp.status() == StatusCode::BAD_REQUEST,
            "unexpected: {}",
            resp.status()
        );
    }

    #[tokio::test]
    async fn list_projections_returns_ok() {
        let resp = app()
            .oneshot(
                Request::get("/admin/default/projections")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn drop_projection_returns_ok() {
        let resp = app()
            .oneshot(
                Request::delete("/admin/default/projections/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn projection_database_not_found() {
        let resp = app()
            .oneshot(
                Request::get("/admin/nonexistent/projections")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // -----------------------------------------------------------------------
    // SHACL validation endpoint
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn validate_shacl_returns_response() {
        let resp = app()
            .oneshot(
                Request::post("/admin/default/validate/shacl")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"shapes_graph":"@prefix sh: <http://www.w3.org/ns/shacl#> ."}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        // 200 when shacl feature enabled, 400 when disabled
        assert!(
            resp.status() == StatusCode::OK || resp.status() == StatusCode::BAD_REQUEST,
            "unexpected: {}",
            resp.status()
        );
    }

    #[tokio::test]
    async fn validate_shacl_database_not_found() {
        let resp = app()
            .oneshot(
                Request::post("/admin/nonexistent/validate/shacl")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"shapes_graph":""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        // 404 or 400 (if shacl feature not enabled, error fires before DB lookup)
        assert!(
            resp.status() == StatusCode::NOT_FOUND || resp.status() == StatusCode::BAD_REQUEST,
            "unexpected: {}",
            resp.status()
        );
    }
}
