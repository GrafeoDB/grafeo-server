//! Grafeo GWP — GQL Wire Protocol (gRPC) transport adapter.
//!
//! Implements the `GqlBackend` trait from the `gwp` crate, bridging
//! GWP sessions to grafeo-engine via `grafeo-service::ServiceState`.
//!
//! Each GWP session maps to one engine session on a specific database.
//! All engine operations run via `spawn_blocking` to avoid blocking
//! the async runtime.

#[cfg(feature = "auth")]
mod auth;
mod backend;
mod encode;

pub use backend::GrafeoBackend;

use std::net::SocketAddr;
use std::time::Duration;

/// Configuration options for the GWP server.
///
/// All fields are optional — `Default` gives a bare server with no
/// TLS, no auth, no idle reaper, and unlimited sessions.
#[derive(Default)]
pub struct GwpOptions {
    /// Idle session timeout. Sessions inactive longer than this are
    /// automatically closed and their transactions rolled back.
    pub idle_timeout: Option<Duration>,

    /// Maximum concurrent GWP sessions. New handshakes are rejected
    /// with `RESOURCE_EXHAUSTED` once the limit is reached.
    pub max_sessions: Option<usize>,

    /// TLS certificate path (PEM). Both cert and key must be set to enable TLS.
    #[cfg(feature = "tls")]
    pub tls_cert: Option<String>,

    /// TLS private key path (PEM).
    #[cfg(feature = "tls")]
    pub tls_key: Option<String>,

    /// Auth provider for handshake credential validation.
    #[cfg(feature = "auth")]
    pub auth_provider: Option<grafeo_service::auth::AuthProvider>,
}

/// Starts the GWP (gRPC) server on the given address.
///
/// Uses the `GqlServer` builder from gwp to configure TLS,
/// authentication, idle timeout, and session limits.
pub async fn serve(
    backend: GrafeoBackend,
    addr: SocketAddr,
    options: GwpOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut builder = gwp::server::GqlServer::builder(backend);

    if let Some(timeout) = options.idle_timeout {
        builder = builder.idle_timeout(timeout);
    }
    if let Some(limit) = options.max_sessions {
        builder = builder.max_sessions(limit);
    }

    #[cfg(feature = "tls")]
    if let (Some(cert_path), Some(key_path)) = (options.tls_cert, options.tls_key) {
        let cert_pem = std::fs::read(&cert_path)
            .map_err(|e| format!("cannot read GWP TLS cert '{cert_path}': {e}"))?;
        let key_pem = std::fs::read(&key_path)
            .map_err(|e| format!("cannot read GWP TLS key '{key_path}': {e}"))?;
        let identity = tonic::transport::Identity::from_pem(&cert_pem, &key_pem);
        let tls_config = tonic::transport::ServerTlsConfig::new().identity(identity);
        builder = builder.tls(tls_config);
    }

    #[cfg(feature = "auth")]
    if let Some(provider) = options.auth_provider {
        builder = builder.auth(auth::GwpAuthValidator::new(provider));
    }

    builder.serve(addr).await?;
    Ok(())
}
