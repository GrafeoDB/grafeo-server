//! Replication status endpoint.
//!
//! `GET /admin/replication` returns the current replication mode and
//! per-database epoch lag.  Always available (returns `{mode: "standalone"}`
//! on instances that have not been configured for replication).

use axum::Json;
use axum::extract::State;

use grafeo_service::replication::ReplicationStatus;

use crate::AppState;
use crate::error::ApiError;
use crate::middleware::auth_context::AuthContext;

/// `GET /admin/replication`
///
/// Returns the replication mode and per-database status. Needs an admin
/// token with no database list, as it reports every database.
pub async fn get_replication_status(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<Json<ReplicationStatus>, ApiError> {
    auth.check_server_admin()?;
    let status = state
        .service()
        .replication_state()
        .status(state.service().replication_mode());
    Ok(Json(status))
}
