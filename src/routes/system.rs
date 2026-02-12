//! System and health endpoints.

use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::state::AppState;

use super::types::{EnabledFeatures, HealthResponse, ResourceDefaults, SystemResources};

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
pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let dbs = state.databases();
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        engine_version: env!("GRAFEO_ENGINE_VERSION").to_string(),
        persistent: dbs.data_dir().is_some(),
        uptime_seconds: state.uptime_secs(),
        active_sessions: dbs.total_active_sessions(),
        features: EnabledFeatures::detect(),
    })
}

/// Get system resource availability.
///
/// Returns total/available memory, disk space, available database types,
/// and default values for the create database dialog.
#[utoipa::path(
    get,
    path = "/system/resources",
    responses(
        (status = 200, description = "System resource info", body = SystemResources),
    ),
    tag = "System"
)]
pub async fn system_resources(State(state): State<AppState>) -> impl IntoResponse {
    use sysinfo::System;

    let sys = System::new_all();
    let total_memory_bytes = sys.total_memory();
    let allocated_memory_bytes = state.databases().total_allocated_memory() as u64;
    let max_available = total_memory_bytes.saturating_mul(80) / 100;
    let available_memory_bytes = max_available.saturating_sub(allocated_memory_bytes);

    let available_disk_bytes = state.databases().data_dir().and_then(|dir| {
        let disks = sysinfo::Disks::new_with_refreshed_list();
        let dir_canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        disks
            .iter()
            .filter(|d| dir_canonical.starts_with(d.mount_point()))
            .max_by_key(|d| d.mount_point().as_os_str().len())
            .map(|d| d.available_space())
    });

    let persistent_available = state.databases().data_dir().is_some();

    #[allow(unused_mut)]
    let mut available_types = vec!["Lpg".to_string(), "Rdf".to_string()];
    #[cfg(feature = "owl-schema")]
    available_types.push("OwlSchema".to_string());
    #[cfg(feature = "rdfs-schema")]
    available_types.push("RdfsSchema".to_string());
    #[cfg(feature = "json-schema")]
    available_types.push("JsonSchema".to_string());

    let num_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    Json(SystemResources {
        total_memory_bytes,
        allocated_memory_bytes,
        available_memory_bytes,
        available_disk_bytes,
        persistent_available,
        available_types,
        defaults: ResourceDefaults {
            memory_limit_bytes: 512 * 1024 * 1024,
            storage_mode: "InMemory".to_string(),
            wal_enabled: false,
            wal_durability: "batch".to_string(),
            backward_edges: true,
            threads: num_cpus,
        },
    })
}

/// Prometheus-compatible metrics endpoint.
pub async fn metrics_endpoint(State(state): State<AppState>) -> impl IntoResponse {
    let dbs = state.databases();
    let db_list = dbs.list();
    let nodes_total: usize = db_list.iter().map(|d| d.node_count).sum();
    let edges_total: usize = db_list.iter().map(|d| d.edge_count).sum();

    let body = state.metrics().render(
        db_list.len(),
        nodes_total,
        edges_total,
        dbs.total_active_sessions(),
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
