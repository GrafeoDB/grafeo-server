//! Grafeo Server entry point.

use std::net::SocketAddr;

use tracing_subscriber::EnvFilter;

use grafeo_server::AppState;
use grafeo_server::config::Config;

#[tokio::main]
async fn main() {
    let config = Config::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.log_level)),
        )
        .init();

    let state = AppState::new(&config);

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        persistent = config.data_dir.is_some(),
        "Grafeo Server starting",
    );

    let app = grafeo_server::router(state.clone());

    let addr = SocketAddr::new(config.host.parse().expect("invalid host"), config.port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind");

    tracing::info!(%addr, "Grafeo Server ready");

    // Spawn session cleanup task
    let cleanup_state = state.clone();
    let session_ttl = config.session_ttl;
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let removed = cleanup_state.databases().cleanup_all_expired(session_ttl);
            if removed > 0 {
                tracing::info!(removed, "Cleaned up expired sessions");
            }
        }
    });

    axum::serve(listener, app)
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
