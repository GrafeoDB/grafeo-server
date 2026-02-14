//! Grafeo Server entry point.
//!
//! Supports multiple transport modes depending on compiled features:
//! - `http`  — REST API on the configured port (default 7474)
//! - `gwp`   — GQL Wire Protocol (gRPC) on the GWP port (default 7687)
//! - both    — HTTP as primary, GWP spawned alongside

use grafeo_server::AppState;
use grafeo_server::config::Config;

use tracing_subscriber::EnvFilter;

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
        "Grafeo Server starting",
    );

    // Spawn session + rate-limiter cleanup task
    let cleanup_state = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let removed = cleanup_state.cleanup_expired_sessions();
            if removed > 0 {
                tracing::info!(removed, "Cleaned up expired sessions");
            }
            cleanup_state.cleanup_rate_limits();
        }
    });

    // -----------------------------------------------------------------
    // GWP-only mode (no HTTP)
    // -----------------------------------------------------------------
    #[cfg(all(feature = "gwp", not(feature = "http")))]
    {
        let gwp_addr = std::net::SocketAddr::new(
            config.host.parse().expect("invalid host"),
            config.gwp_port,
        );
        let backend = grafeo_server::gwp::GrafeoBackend::new(state);
        tracing::info!(%gwp_addr, "GWP server ready (standalone)");
        if let Err(e) = gwp::server::GqlServer::serve(backend, gwp_addr).await {
            tracing::error!("GWP server error: {e}");
        }
        tracing::info!("Grafeo Server shut down");
        return;
    }

    // -----------------------------------------------------------------
    // HTTP mode (optionally with GWP alongside)
    // -----------------------------------------------------------------
    #[cfg(feature = "http")]
    {
        let addr = std::net::SocketAddr::new(
            config.host.parse().expect("invalid host"),
            config.port,
        );
        let app = grafeo_server::router(state.clone());
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .expect("failed to bind");

        // Spawn GWP alongside HTTP if both features are enabled
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
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");

        tracing::info!("Grafeo Server shut down");
    }

    // If neither transport is enabled, just warn and exit
    #[cfg(not(any(feature = "http", feature = "gwp")))]
    {
        tracing::error!("No transport features enabled. Enable 'http' and/or 'gwp'.");
    }
}

#[cfg(feature = "http")]
async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install signal handler");
    tracing::info!("Shutdown signal received");
}
