//! Grafeo Server - HTTP server for the Grafeo graph database.
//!
//! Wraps the Grafeo engine in an HTTP API with support for auto-commit
//! queries, explicit transactions, and an embedded web UI.

#[cfg(feature = "auth")]
pub mod auth;
pub mod config;
pub mod database_manager;
pub mod error;
pub mod metrics;
pub mod rate_limit;
pub mod request_id;
pub mod routes;
pub mod schema;
pub mod sessions;
pub mod state;
#[cfg(feature = "tls")]
pub mod tls;
mod ui;

pub use routes::router;
pub use state::AppState;
