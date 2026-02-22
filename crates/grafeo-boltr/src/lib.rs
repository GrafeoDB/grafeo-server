//! Grafeo BoltR — Bolt v5 wire protocol transport adapter.
//!
//! Implements the `BoltBackend` trait from the `boltr` crate, bridging
//! Bolt sessions to grafeo-engine via `grafeo-service::ServiceState`.

#[cfg(feature = "auth")]
mod auth;
mod backend;
mod encode;

pub use backend::GrafeoBackend;

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

/// Options for configuring the Bolt server.
#[derive(Default)]
pub struct BoltrOptions {
    pub idle_timeout: Option<Duration>,
    pub max_sessions: Option<usize>,
    #[cfg(feature = "tls")]
    pub tls_cert: Option<String>,
    #[cfg(feature = "tls")]
    pub tls_key: Option<String>,
    #[cfg(feature = "auth")]
    pub auth_provider: Option<grafeo_service::auth::AuthProvider>,
    pub shutdown: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
}

/// Starts the Bolt server with the given backend and options.
pub async fn serve(
    backend: GrafeoBackend,
    addr: SocketAddr,
    options: BoltrOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut builder = boltr::server::BoltServer::builder(backend);

    if let Some(timeout) = options.idle_timeout {
        builder = builder.idle_timeout(timeout);
    }
    if let Some(limit) = options.max_sessions {
        builder = builder.max_sessions(limit);
    }

    #[cfg(feature = "auth")]
    if let Some(provider) = options.auth_provider {
        builder = builder.auth(auth::BoltrAuthValidator::new(provider));
    }

    if let Some(signal) = options.shutdown {
        builder = builder.shutdown(signal);
    }

    builder.serve(addr).await?;
    Ok(())
}
