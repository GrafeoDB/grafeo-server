//! Grafeo Server — graph database server with pluggable transports.
//!
//! Core business logic (database management, query execution, sessions,
//! metrics, authentication) lives in the `grafeo-service` crate.
//!
//! This crate provides transport adapters:
//! - `http`   — REST API via axum (includes Swagger UI)
//! - `studio` — embedded web UI via rust-embed (requires `http`)
//! - `gwp`    — GQL Wire Protocol via gRPC

#[cfg(feature = "auth")]
pub mod auth;
pub mod config;
pub mod error;
#[cfg(feature = "gwp")]
pub mod gwp;
#[cfg(feature = "http")]
pub mod rate_limit;
#[cfg(feature = "http")]
pub mod request_id;
#[cfg(feature = "http")]
pub mod routes;
pub mod state;
#[cfg(feature = "tls")]
pub mod tls;
#[cfg(feature = "studio")]
mod ui;

pub use grafeo_service;
#[cfg(feature = "http")]
pub use routes::router;
pub use state::AppState;
