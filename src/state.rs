//! Shared application state: database manager and server metadata.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::database_manager::DatabaseManager;
use crate::metrics::Metrics;
use crate::rate_limit::RateLimiter;

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
    #[cfg(feature = "auth")]
    auth_token: Option<String>,
    #[cfg(feature = "auth")]
    auth_user: Option<String>,
    #[cfg(feature = "auth")]
    auth_password: Option<String>,
    rate_limiter: RateLimiter,
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
                #[cfg(feature = "auth")]
                auth_token: config.auth_token.clone(),
                #[cfg(feature = "auth")]
                auth_user: config.auth_user.clone(),
                #[cfg(feature = "auth")]
                auth_password: config.auth_password.clone(),
                rate_limiter: RateLimiter::new(
                    config.rate_limit,
                    Duration::from_secs(config.rate_limit_window),
                ),
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
                #[cfg(feature = "auth")]
                auth_token: None,
                #[cfg(feature = "auth")]
                auth_user: None,
                #[cfg(feature = "auth")]
                auth_password: None,
                rate_limiter: RateLimiter::new(0, Duration::from_secs(60)),
            }),
        }
    }

    /// Creates an in-memory state with token authentication enabled (for tests).
    #[cfg(feature = "auth")]
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
                auth_user: None,
                auth_password: None,
                rate_limiter: RateLimiter::new(0, Duration::from_secs(60)),
            }),
        }
    }

    /// Creates an in-memory state with basic auth enabled (for tests).
    #[cfg(feature = "auth")]
    pub fn new_in_memory_with_basic_auth(session_ttl: u64, user: String, password: String) -> Self {
        Self {
            inner: Arc::new(Inner {
                databases: DatabaseManager::new(None),
                session_ttl,
                cors_origins: vec![],
                start_time: Instant::now(),
                metrics: Metrics::new(),
                query_timeout: Duration::from_secs(30),
                auth_token: None,
                auth_user: Some(user),
                auth_password: Some(password),
                rate_limiter: RateLimiter::new(0, Duration::from_secs(60)),
            }),
        }
    }

    /// Creates an in-memory state with rate limiting enabled (for tests).
    pub fn new_in_memory_with_rate_limit(
        session_ttl: u64,
        max_requests: u64,
        window: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                databases: DatabaseManager::new(None),
                session_ttl,
                cors_origins: vec![],
                start_time: Instant::now(),
                metrics: Metrics::new(),
                query_timeout: Duration::from_secs(30),
                #[cfg(feature = "auth")]
                auth_token: None,
                #[cfg(feature = "auth")]
                auth_user: None,
                #[cfg(feature = "auth")]
                auth_password: None,
                rate_limiter: RateLimiter::new(max_requests, window),
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
    #[cfg(feature = "auth")]
    pub fn auth_token(&self) -> Option<&str> {
        self.inner.auth_token.as_deref()
    }

    /// Returns the configured basic auth credentials, if both are set.
    #[cfg(feature = "auth")]
    pub fn basic_auth(&self) -> Option<(&str, &str)> {
        match (&self.inner.auth_user, &self.inner.auth_password) {
            (Some(user), Some(pass)) => Some((user.as_str(), pass.as_str())),
            _ => None,
        }
    }

    /// Returns true if any authentication method is configured.
    #[cfg(feature = "auth")]
    pub fn has_auth(&self) -> bool {
        self.inner.auth_token.is_some() || self.basic_auth().is_some()
    }

    /// Returns a reference to the rate limiter.
    pub fn rate_limiter(&self) -> &RateLimiter {
        &self.inner.rate_limiter
    }
}
