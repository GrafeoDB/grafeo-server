//! TLS configuration and HTTPS serving.
//!
//! Enabled only when the `tls` Cargo feature is active.

use std::fs::File;
use std::io::BufReader;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::ConnectInfo;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use rustls::ServerConfig;
use rustls_pemfile::{certs, private_key};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

/// Loads TLS certificate chain and private key from PEM files.
pub fn load_rustls_config(cert_path: &str, key_path: &str) -> Result<ServerConfig, String> {
    // Install ring as the default crypto provider (idempotent).
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cert_file =
        File::open(cert_path).map_err(|e| format!("cannot open TLS cert '{cert_path}': {e}"))?;
    let key_file =
        File::open(key_path).map_err(|e| format!("cannot open TLS key '{key_path}': {e}"))?;

    let certs: Vec<_> = certs(&mut BufReader::new(cert_file))
        .collect::<Result<_, _>>()
        .map_err(|e| format!("invalid certificate: {e}"))?;
    if certs.is_empty() {
        return Err("no certificates found in cert file".into());
    }

    let key = private_key(&mut BufReader::new(key_file))
        .map_err(|e| format!("invalid private key: {e}"))?
        .ok_or("no private key found in key file")?;

    ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("TLS configuration error: {e}"))
}

/// Serves the application over TLS.
///
/// Injects `ConnectInfo<SocketAddr>` into each request so that
/// rate limiting and IP-based middleware continue to work.
pub async fn serve_tls(
    listener: TcpListener,
    config: ServerConfig,
    app: Router,
    shutdown: impl std::future::Future<Output = ()>,
) {
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let mut shutdown = std::pin::pin!(shutdown);

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (tcp, addr) = match result {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("TCP accept error: {e}");
                        continue;
                    }
                };

                let acceptor = acceptor.clone();
                let app = app.clone();

                tokio::spawn(async move {
                    let tls = match acceptor.accept(tcp).await {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::debug!(%addr, "TLS handshake failed: {e}");
                            return;
                        }
                    };

                    serve_connection(tls, addr, app).await;
                });
            }
            () = &mut shutdown => {
                tracing::info!("Shutdown signal received");
                break;
            }
        }
    }
}

/// Serves a single TLS connection using hyper, injecting `ConnectInfo`.
async fn serve_connection(
    tls: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    addr: SocketAddr,
    app: Router,
) {
    let io = TokioIo::new(tls);
    let svc = app.into_service();

    let hyper_svc =
        hyper::service::service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
            req.extensions_mut().insert(ConnectInfo(addr));
            let mut svc = svc.clone();
            async move { tower::Service::call(&mut svc, req).await }
        });

    if let Err(e) = Builder::new(TokioExecutor::new())
        .serve_connection_with_upgrades(io, hyper_svc)
        .await
    {
        tracing::debug!(%addr, "Connection error: {e}");
    }
}
