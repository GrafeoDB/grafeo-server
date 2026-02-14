//! Grafeo Server - graph database server with pluggable transports.
//!
//! The core (database management, sessions, metrics, schema) is always compiled.
//! Transport layers are feature-gated:
//! - `http`   — REST API via axum (includes Swagger UI)
//! - `studio` — embedded web UI via rust-embed (requires `http`)
//! - `gwp`    — GQL Wire Protocol via gRPC

#[cfg(feature = "auth")]
pub mod auth;
pub mod config;
pub mod database_manager;
pub mod error;
#[cfg(feature = "gwp")]
pub mod gwp;
pub mod metrics;
pub mod rate_limit;
#[cfg(feature = "http")]
pub mod request_id;
#[cfg(feature = "http")]
pub mod routes;
pub mod schema;
pub mod sessions;
pub mod state;
#[cfg(feature = "tls")]
pub mod tls;
pub mod types;
#[cfg(feature = "studio")]
mod ui;

#[cfg(feature = "http")]
pub use routes::router;
pub use state::AppState;
