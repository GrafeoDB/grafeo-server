//! Shared application state: database manager and server metadata.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::database_manager::DatabaseManager;
use crate::metrics::Metrics;

/// Shared application state, cloneable across handlers.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    databases: DatabaseManager,
    session_ttl: u64,
    cors_origins: Vec<String>,
    start_time: Instant,
    metrics: Metrics,
    query_timeout: Duration,
    auth_token: Option<String>,
}

impl AppState {
    /// Creates a new application state from config.
    pub fn new(config: &Config) -> Self {
        let databases = DatabaseManager::new(config.data_dir.as_deref());

        Self {
            inner: Arc::new(Inner {
                databases,
                session_ttl: config.session_ttl,
                cors_origins: config.cors_origins.clone(),
                start_time: Instant::now(),
                metrics: Metrics::new(),
                query_timeout: Duration::from_secs(config.query_timeout),
                auth_token: config.auth_token.clone(),
            }),
        }
    }

    /// Creates an in-memory application state (for tests and ephemeral use).
    pub fn new_in_memory(session_ttl: u64) -> Self {
        Self {
            inner: Arc::new(Inner {
                databases: DatabaseManager::new(None),
                session_ttl,
                cors_origins: vec![],
                start_time: Instant::now(),
                metrics: Metrics::new(),
                query_timeout: Duration::from_secs(30),
                auth_token: None,
            }),
        }
    }

    /// Creates an in-memory state with authentication enabled (for tests).
    pub fn new_in_memory_with_auth(session_ttl: u64, auth_token: String) -> Self {
        Self {
            inner: Arc::new(Inner {
                databases: DatabaseManager::new(None),
                session_ttl,
                cors_origins: vec![],
                start_time: Instant::now(),
                metrics: Metrics::new(),
                query_timeout: Duration::from_secs(30),
                auth_token: Some(auth_token),
            }),
        }
    }

    /// Returns a reference to the database manager.
    pub fn databases(&self) -> &DatabaseManager {
        &self.inner.databases
    }

    /// Returns the configured session TTL in seconds.
    pub fn session_ttl(&self) -> u64 {
        self.inner.session_ttl
    }

    /// Returns the configured CORS allowed origins.
    pub fn cors_origins(&self) -> &[String] {
        &self.inner.cors_origins
    }

    /// Returns the server uptime in seconds.
    pub fn uptime_secs(&self) -> u64 {
        self.inner.start_time.elapsed().as_secs()
    }

    /// Returns a reference to the metrics collector.
    pub fn metrics(&self) -> &Metrics {
        &self.inner.metrics
    }

    /// Returns the global query timeout (Duration::ZERO means disabled).
    pub fn query_timeout(&self) -> Duration {
        self.inner.query_timeout
    }

    /// Returns the configured auth token, if any.
    pub fn auth_token(&self) -> Option<&str> {
        self.inner.auth_token.as_deref()
    }
}
