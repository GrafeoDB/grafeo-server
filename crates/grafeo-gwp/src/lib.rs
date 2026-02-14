//! Grafeo GWP — GQL Wire Protocol (gRPC) transport adapter.
//!
//! Implements the `GqlBackend` trait from the `gwp` crate, bridging
//! GWP sessions to grafeo-engine via `grafeo-service::ServiceState`.
//!
//! Each GWP session maps to one engine session on a specific database.
//! All engine operations run via `spawn_blocking` to avoid blocking
//! the async runtime.

mod backend;
mod encode;

pub use backend::GrafeoBackend;

use std::net::SocketAddr;

/// Starts the GWP (gRPC) server on the given address.
///
/// This is a convenience wrapper around `gwp::server::GqlServer::serve`.
/// The future resolves when the server shuts down.
pub async fn serve(
    backend: GrafeoBackend,
    addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    gwp::server::GqlServer::serve(backend, addr).await?;
    Ok(())
}
