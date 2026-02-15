//! Grafeo Server — graph database server with pluggable transports.
//!
//! This is the binary crate. Core business logic lives in `grafeo-service`.
//! Transport adapters live in `grafeo-http`, `grafeo-gwp`, `grafeo-studio`.
//!
//! This module re-exports key types for integration tests and backwards compat.

pub mod config;

pub use grafeo_service;

// Re-export HTTP types for integration tests
#[cfg(feature = "http")]
pub use grafeo_http::{AppState, router};
