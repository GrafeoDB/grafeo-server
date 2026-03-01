//! Grafeo Server entry point.
//!
//! Supports multiple transport modes depending on compiled features:
//! - `http`  — REST API on the configured port (default 7474)
//! - `gwp`   — GQL Wire Protocol (gRPC) on the GWP port (default 7688)
//! - `bolt`  — BoltR (Bolt v5.x) on the Bolt port (default 7687)
//! - any combination — HTTP as primary, GWP/BoltR spawned alongside

use grafeo_server::config::Config;

#[cfg(feature = "http")]
use grafeo_service::types::EnabledFeatures;
use grafeo_service::{ServiceConfig, ServiceState};
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

    let service_config = ServiceConfig {
        data_dir: config.data_dir.clone(),
        session_ttl: config.session_ttl,
        query_timeout: config.query_timeout,
        rate_limit: config.rate_limit,
        rate_limit_window: config.rate_limit_window,
        #[cfg(feature = "auth")]
        auth_token: config.auth_token.clone(),
        #[cfg(feature = "auth")]
        auth_user: config.auth_user.clone(),
        #[cfg(feature = "auth")]
        auth_password: config.auth_password.clone(),
    };

    let service = ServiceState::new(&service_config);

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        persistent = config.data_dir.is_some(),
        "Grafeo Server starting",
    );

    // Spawn session + rate-limiter cleanup task
    let cleanup_state = service.clone();
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
    // Standalone mode: GWP and/or BoltR only (no HTTP)
    // -----------------------------------------------------------------
    #[cfg(all(any(feature = "gwp", feature = "bolt"), not(feature = "http")))]
    {
        let host: std::net::IpAddr = config.host.parse().expect("invalid host");

        #[cfg(feature = "gwp")]
        let gwp_handle = {
            let gwp_state = service.clone();
            let gwp_addr = std::net::SocketAddr::new(host, config.gwp_port);
            let mut gwp_options = build_gwp_options(&config, &service);
            gwp_options.shutdown = Some(Box::pin(async {
                tokio::signal::ctrl_c().await.ok();
            }));
            tokio::spawn(async move {
                let backend = grafeo_gwp::GrafeoBackend::new(gwp_state);
                tracing::info!(%gwp_addr, "GWP (gRPC) server ready (standalone)");
                if let Err(e) = grafeo_gwp::serve(backend, gwp_addr, gwp_options).await {
                    tracing::error!("GWP server error: {e}");
                }
            })
        };

        #[cfg(feature = "bolt")]
        let bolt_handle = {
            let bolt_state = service.clone();
            let bolt_addr = std::net::SocketAddr::new(host, config.bolt_port);
            let mut bolt_options = build_bolt_options(&config, &service);
            bolt_options.shutdown = Some(Box::pin(async {
                tokio::signal::ctrl_c().await.ok();
            }));
            tokio::spawn(async move {
                let backend =
                    grafeo_boltr::GrafeoBackend::new(bolt_state).with_advertise_addr(bolt_addr);
                tracing::info!(%bolt_addr, "BoltR server ready (standalone)");
                if let Err(e) = grafeo_boltr::serve(backend, bolt_addr, bolt_options).await {
                    tracing::error!("BoltR server error: {e}");
                }
            })
        };

        // Wait for shutdown signal, then await handles
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install signal handler");
        tracing::info!("Shutdown signal received");

        #[cfg(feature = "gwp")]
        gwp_handle.await.ok();
        #[cfg(feature = "bolt")]
        bolt_handle.await.ok();

        tracing::info!("Grafeo Server shut down");
        return;
    }

    // -----------------------------------------------------------------
    // HTTP mode (optionally with GWP and/or BoltR alongside)
    // -----------------------------------------------------------------
    #[cfg(feature = "http")]
    {
        let addr =
            std::net::SocketAddr::new(config.host.parse().expect("invalid host"), config.port);

        let features = detect_features();
        let app_state =
            grafeo_http::AppState::new(service.clone(), config.cors_origins.clone(), features);

        let mut app = grafeo_http::router(app_state);

        // Merge Studio UI routes if enabled
        #[cfg(feature = "studio")]
        {
            app = grafeo_studio::router().merge(app);
        }

        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .expect("failed to bind");

        // Spawn GWP alongside HTTP if both features are enabled
        #[cfg(feature = "gwp")]
        let gwp_handle = {
            let gwp_state = service.clone();
            let mut gwp_options = build_gwp_options(&config, &service);
            let gwp_addr = std::net::SocketAddr::new(addr.ip(), config.gwp_port);
            gwp_options.shutdown = Some(Box::pin(async {
                tokio::signal::ctrl_c().await.ok();
            }));
            tokio::spawn(async move {
                let backend = grafeo_gwp::GrafeoBackend::new(gwp_state);
                tracing::info!(%gwp_addr, "GWP (gRPC) server ready");
                if let Err(e) = grafeo_gwp::serve(backend, gwp_addr, gwp_options).await {
                    tracing::error!("GWP server error: {e}");
                }
            })
        };

        // Spawn BoltR alongside HTTP if both features are enabled
        #[cfg(feature = "bolt")]
        let bolt_handle = {
            let bolt_state = service.clone();
            let mut bolt_options = build_bolt_options(&config, &service);
            let bolt_addr = std::net::SocketAddr::new(addr.ip(), config.bolt_port);
            bolt_options.shutdown = Some(Box::pin(async {
                tokio::signal::ctrl_c().await.ok();
            }));
            tokio::spawn(async move {
                let backend =
                    grafeo_boltr::GrafeoBackend::new(bolt_state).with_advertise_addr(bolt_addr);
                tracing::info!(%bolt_addr, "BoltR server ready");
                if let Err(e) = grafeo_boltr::serve(backend, bolt_addr, bolt_options).await {
                    tracing::error!("BoltR server error: {e}");
                }
            })
        };

        #[cfg(feature = "tls")]
        if config.tls_enabled() {
            let tls_config = grafeo_http::tls::load_rustls_config(
                config.tls_cert.as_ref().unwrap(),
                config.tls_key.as_ref().unwrap(),
            )
            .expect("failed to load TLS configuration");

            tracing::info!(%addr, "Grafeo Server ready (HTTPS)");

            grafeo_http::tls::serve_tls(listener, tls_config, app, shutdown_signal()).await;

            #[cfg(feature = "gwp")]
            gwp_handle.await.ok();
            #[cfg(feature = "bolt")]
            bolt_handle.await.ok();

            tracing::info!("Grafeo Server shut down");
            return;
        }

        tracing::info!(%addr, "Grafeo Server ready");

        grafeo_http::serve(listener, app, shutdown_signal()).await;

        #[cfg(feature = "gwp")]
        gwp_handle.await.ok();
        #[cfg(feature = "bolt")]
        bolt_handle.await.ok();

        tracing::info!("Grafeo Server shut down");
    }

    // If no transport is enabled, just warn and exit
    #[cfg(not(any(feature = "http", feature = "gwp", feature = "bolt")))]
    {
        tracing::error!("No transport features enabled. Enable 'http', 'gwp', and/or 'bolt'.");
    }
}

/// Detects compiled feature flags at build time.
///
/// This function runs in the binary crate which has all feature flags,
/// so `cfg!()` checks work correctly. The result is passed to transport
/// crates that need to report enabled features (e.g. health endpoint).
#[cfg(feature = "http")]
fn detect_features() -> EnabledFeatures {
    let mut languages = Vec::new();
    if cfg!(feature = "gql") {
        languages.push("gql".to_string());
    }
    if cfg!(feature = "cypher") {
        languages.push("cypher".to_string());
    }
    if cfg!(feature = "sparql") {
        languages.push("sparql".to_string());
    }
    if cfg!(feature = "gremlin") {
        languages.push("gremlin".to_string());
    }
    if cfg!(feature = "graphql") {
        languages.push("graphql".to_string());
    }
    if cfg!(feature = "sql-pgq") {
        languages.push("sql-pgq".to_string());
    }

    let mut engine = Vec::new();
    if cfg!(feature = "parallel") {
        engine.push("parallel".to_string());
    }
    if cfg!(feature = "wal") {
        engine.push("wal".to_string());
    }
    if cfg!(feature = "spill") {
        engine.push("spill".to_string());
    }
    if cfg!(feature = "mmap") {
        engine.push("mmap".to_string());
    }
    if cfg!(feature = "rdf") {
        engine.push("rdf".to_string());
    }
    if cfg!(feature = "vector-index") {
        engine.push("vector-index".to_string());
    }
    if cfg!(feature = "text-index") {
        engine.push("text-index".to_string());
    }
    if cfg!(feature = "hybrid-search") {
        engine.push("hybrid-search".to_string());
    }
    if cfg!(feature = "cdc") {
        engine.push("cdc".to_string());
    }
    if cfg!(feature = "embed") {
        engine.push("embed".to_string());
    }

    let mut server = Vec::new();
    if cfg!(feature = "auth") {
        server.push("auth".to_string());
    }
    if cfg!(feature = "tls") {
        server.push("tls".to_string());
    }
    if cfg!(feature = "owl-schema") {
        server.push("owl-schema".to_string());
    }
    if cfg!(feature = "rdfs-schema") {
        server.push("rdfs-schema".to_string());
    }
    if cfg!(feature = "json-schema") {
        server.push("json-schema".to_string());
    }
    if cfg!(feature = "gwp") {
        server.push("gwp".to_string());
    }
    if cfg!(feature = "bolt") {
        server.push("bolt".to_string());
    }

    EnabledFeatures {
        languages,
        engine,
        server,
    }
}

/// Builds GWP server options from CLI config and service state.
#[cfg(feature = "gwp")]
fn build_gwp_options(config: &Config, service: &ServiceState) -> grafeo_gwp::GwpOptions {
    let _ = service; // used only when auth feature is enabled

    grafeo_gwp::GwpOptions {
        idle_timeout: Some(std::time::Duration::from_secs(config.session_ttl)),
        max_sessions: if config.gwp_max_sessions > 0 {
            Some(config.gwp_max_sessions)
        } else {
            None
        },
        #[cfg(feature = "tls")]
        tls_cert: config.tls_cert.clone(),
        #[cfg(feature = "tls")]
        tls_key: config.tls_key.clone(),
        #[cfg(feature = "auth")]
        auth_provider: service.auth().cloned(),
        shutdown: None,
    }
}

/// Builds BoltR server options from CLI config and service state.
#[cfg(feature = "bolt")]
fn build_bolt_options(config: &Config, service: &ServiceState) -> grafeo_boltr::BoltrOptions {
    let _ = service; // used only when auth feature is enabled

    grafeo_boltr::BoltrOptions {
        idle_timeout: Some(std::time::Duration::from_secs(config.session_ttl)),
        max_sessions: if config.bolt_max_sessions > 0 {
            Some(config.bolt_max_sessions)
        } else {
            None
        },
        #[cfg(feature = "tls")]
        tls_cert: config.tls_cert.clone(),
        #[cfg(feature = "tls")]
        tls_key: config.tls_key.clone(),
        #[cfg(feature = "auth")]
        auth_provider: service.auth().cloned(),
        shutdown: None,
    }
}

#[cfg(feature = "http")]
async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install signal handler");
    tracing::info!("Shutdown signal received");
}
