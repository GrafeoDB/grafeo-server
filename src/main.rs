//! Grafeo Server entry point.

use std::net::SocketAddr;

use tracing_subscriber::EnvFilter;

use grafeo_server::AppState;
use grafeo_server::config::Config;

#[tokio::main]
async fn main() {
    let config = Config::parse();

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.log_level));

    if config.log_format == "json" {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    }

    let state = AppState::new(&config);

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        persistent = config.data_dir.is_some(),
        tls = config.tls_enabled(),
        "Grafeo Server starting",
    );

    let app = grafeo_server::router(state.clone());

    let addr = SocketAddr::new(config.host.parse().expect("invalid host"), config.port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind");

    // Spawn session + rate-limiter cleanup task
    let cleanup_state = state.clone();
    let session_ttl = config.session_ttl;
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let removed = cleanup_state.databases().cleanup_all_expired(session_ttl);
            if removed > 0 {
                tracing::info!(removed, "Cleaned up expired sessions");
            }
            cleanup_state.rate_limiter().cleanup();
        }
    });

    // Spawn the GQL Wire Protocol (gRPC) server alongside HTTP
    #[cfg(feature = "gwp")]
    {
        let gwp_state = state.clone();
        let gwp_addr = std::net::SocketAddr::new(addr.ip(), config.gwp_port);
        tokio::spawn(async move {
            let backend = grafeo_server::gwp::GrafeoBackend::new(gwp_state);
            tracing::info!(%gwp_addr, "GWP (gRPC) server ready");
            if let Err(e) = gwp::server::GqlServer::serve(backend, gwp_addr).await {
                tracing::error!("GWP server error: {e}");
            }
        });
    }

    #[cfg(feature = "tls")]
    if config.tls_enabled() {
        let tls_config = grafeo_server::tls::load_rustls_config(
            config.tls_cert.as_ref().unwrap(),
            config.tls_key.as_ref().unwrap(),
        )
        .expect("failed to load TLS configuration");

        tracing::info!(%addr, "Grafeo Server ready (HTTPS)");

        grafeo_server::tls::serve_tls(listener, tls_config, app, shutdown_signal()).await;

        tracing::info!("Grafeo Server shut down");
        return;
    }

    tracing::info!(%addr, "Grafeo Server ready");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .expect("server error");

    tracing::info!("Grafeo Server shut down");
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install signal handler");
    tracing::info!("Shutdown signal received");
}
