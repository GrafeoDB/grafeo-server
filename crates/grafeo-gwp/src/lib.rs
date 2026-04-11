//! Grafeo GWP: GQL Wire Protocol (gRPC) transport adapter.
//!
//! Implements the [`GqlBackend`](gwp::server::GqlBackend) trait from the
//! `gwp` crate, bridging GWP sessions to grafeo-engine via
//! `grafeo-service::ServiceState`.
//!
//! Each GWP session maps to one engine session on a specific database.
//! All engine operations run via `spawn_blocking` to avoid blocking
//! the async runtime.
//!
//! # Usage
//!
//! ```rust,ignore
//! use std::net::SocketAddr;
//! use std::time::Duration;
//! use grafeo_gwp::{GrafeoBackend, GwpOptions, serve};
//!
//! let state = /* grafeo_service::ServiceState */;
//! let backend = GrafeoBackend::new(state);
//! let addr: SocketAddr = "0.0.0.0:7688".parse().unwrap();
//!
//! serve(backend, addr, GwpOptions {
//!     idle_timeout: Some(Duration::from_secs(600)),
//!     max_sessions: Some(128),
//!     ..Default::default()
//! }).await?;
//! ```

#[cfg(feature = "auth")]
mod auth;
mod backend;
mod encode;

pub use backend::GrafeoBackend;

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

/// Configuration options for the GWP server.
///
/// All fields are optional: `Default` gives a bare server with no
/// TLS, no auth, no idle reaper, and unlimited sessions.
///
/// ```rust,ignore
/// use std::time::Duration;
/// use grafeo_gwp::GwpOptions;
///
/// let opts = GwpOptions {
///     idle_timeout: Some(Duration::from_secs(600)),
///     max_sessions: Some(128),
///     ..Default::default()
/// };
/// ```
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

    /// Shutdown signal. When the future resolves, the server stops
    /// accepting connections, drains in-flight requests, and stops
    /// the idle session reaper.
    pub shutdown: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
}

/// Starts the GWP (gRPC) server on the given address.
///
/// Uses the `GqlServer` builder from the `gwp` crate to configure TLS,
/// authentication, idle timeout, and session limits.
///
/// ```rust,ignore
/// use grafeo_gwp::{GrafeoBackend, GwpOptions, serve};
///
/// let backend = GrafeoBackend::new(service_state);
/// let addr = "0.0.0.0:7688".parse().unwrap();
/// serve(backend, addr, GwpOptions::default()).await?;
/// ```
pub async fn serve(
    backend: GrafeoBackend,
    addr: SocketAddr,
    options: GwpOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Extract the pending auth map before the builder consumes the backend.
    #[cfg(feature = "auth")]
    let pending = backend.pending.clone();

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

    // Keep the reaper handle alive until serve() returns so it can be
    // aborted when the runtime shuts down.
    #[cfg(feature = "auth")]
    let _reaper: Option<tokio::task::JoinHandle<()>>;

    #[cfg(feature = "auth")]
    if let Some(provider) = options.auth_provider {
        let pending_clone = pending.clone();
        builder = builder.auth(auth::GwpAuthValidator::new(provider, pending_clone));

        // Reap stale auth nonces from clients that never completed session creation.
        _reaper = Some(grafeo_service::auth::spawn_pending_auth_reaper(
            pending,
            std::time::Duration::from_secs(60),
            std::time::Duration::from_secs(30),
        ));
    } else {
        _reaper = None;
    }

    if let Some(signal) = options.shutdown {
        builder = builder.shutdown(signal);
    }

    builder.serve(addr).await?;
    Ok(())
}
