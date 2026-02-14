//! Grafeo Service — core business logic for the Grafeo graph database server.
//!
//! This crate contains all transport-agnostic business logic:
//! database management, query execution, session tracking, metrics,
//! authentication, and schema loading.
//!
//! Transport crates (`grafeo-http`, `grafeo-gwp`, `grafeo-bolt`) depend
//! on this crate and provide protocol-specific adapters.
//!
//! **Zero transport dependencies** — no axum, no tonic, no wire-protocol code.

#[cfg(feature = "auth")]
pub mod auth;
pub mod database;
pub mod error;
pub mod metrics;
pub mod query;
pub mod rate_limit;
pub mod schema;
pub mod session;
pub mod types;

use std::sync::Arc;
use std::time::{Duration, Instant};

use database::DatabaseManager;

use metrics::Metrics;
use rate_limit::RateLimiter;
use session::SessionRegistry;

/// Configuration subset relevant to the service layer.
///
/// Transport-specific config (ports, TLS paths, CORS origins) stays in
/// the binary crate's `Config` struct.
pub struct ServiceConfig {
    pub data_dir: Option<String>,
    pub session_ttl: u64,
    pub query_timeout: u64,
    pub rate_limit: u64,
    pub rate_limit_window: u64,
    #[cfg(feature = "auth")]
    pub auth_token: Option<String>,
    #[cfg(feature = "auth")]
    pub auth_user: Option<String>,
    #[cfg(feature = "auth")]
    pub auth_password: Option<String>,
}

/// Shared service state, cloneable across all transport handlers.
///
/// Wraps all business-layer components in an `Arc`. Transport crates
/// receive this and delegate all logic to it.
#[derive(Clone)]
pub struct ServiceState {
    inner: Arc<Inner>,
}

struct Inner {
    databases: DatabaseManager,
    sessions: SessionRegistry,
    metrics: Metrics,
    rate_limiter: RateLimiter,
    session_ttl: u64,
    query_timeout: Duration,
    start_time: Instant,
    #[cfg(feature = "auth")]
    auth: Option<auth::AuthProvider>,
}

impl ServiceState {
    /// Creates a new service state from config.
    pub fn new(config: &ServiceConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                databases: DatabaseManager::new(config.data_dir.as_deref()),
                sessions: SessionRegistry::new(),
                metrics: Metrics::new(),
                rate_limiter: RateLimiter::new(
                    config.rate_limit,
                    Duration::from_secs(config.rate_limit_window),
                ),
                session_ttl: config.session_ttl,
                query_timeout: Duration::from_secs(config.query_timeout),
                start_time: Instant::now(),
                #[cfg(feature = "auth")]
                auth: auth::AuthProvider::new(
                    config.auth_token.clone(),
                    config.auth_user.clone(),
                    config.auth_password.clone(),
                ),
            }),
        }
    }

    /// Creates an in-memory service state (for tests and ephemeral use).
    pub fn new_in_memory(session_ttl: u64) -> Self {
        Self {
            inner: Arc::new(Inner {
                databases: DatabaseManager::new(None),
                sessions: SessionRegistry::new(),
                metrics: Metrics::new(),
                rate_limiter: RateLimiter::new(0, Duration::from_secs(60)),
                session_ttl,
                query_timeout: Duration::from_secs(30),
                start_time: Instant::now(),
                #[cfg(feature = "auth")]
                auth: None,
            }),
        }
    }

    /// Creates an in-memory state with token authentication enabled (for tests).
    #[cfg(feature = "auth")]
    pub fn new_in_memory_with_auth(session_ttl: u64, auth_token: String) -> Self {
        Self {
            inner: Arc::new(Inner {
                databases: DatabaseManager::new(None),
                sessions: SessionRegistry::new(),
                metrics: Metrics::new(),
                rate_limiter: RateLimiter::new(0, Duration::from_secs(60)),
                session_ttl,
                query_timeout: Duration::from_secs(30),
                start_time: Instant::now(),
                auth: auth::AuthProvider::new(Some(auth_token), None, None),
            }),
        }
    }

    /// Creates an in-memory state with basic auth enabled (for tests).
    #[cfg(feature = "auth")]
    pub fn new_in_memory_with_basic_auth(
        session_ttl: u64,
        user: String,
        password: String,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                databases: DatabaseManager::new(None),
                sessions: SessionRegistry::new(),
                metrics: Metrics::new(),
                rate_limiter: RateLimiter::new(0, Duration::from_secs(60)),
                session_ttl,
                query_timeout: Duration::from_secs(30),
                start_time: Instant::now(),
                auth: auth::AuthProvider::new(None, Some(user), Some(password)),
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
                sessions: SessionRegistry::new(),
                metrics: Metrics::new(),
                rate_limiter: RateLimiter::new(max_requests, window),
                session_ttl,
                query_timeout: Duration::from_secs(30),
                start_time: Instant::now(),
                #[cfg(feature = "auth")]
                auth: None,
            }),
        }
    }

    // --- Accessors ---

    pub fn databases(&self) -> &DatabaseManager {
        &self.inner.databases
    }

    pub fn sessions(&self) -> &SessionRegistry {
        &self.inner.sessions
    }

    pub fn metrics(&self) -> &Metrics {
        &self.inner.metrics
    }

    pub fn rate_limiter(&self) -> &RateLimiter {
        &self.inner.rate_limiter
    }

    pub fn session_ttl(&self) -> u64 {
        self.inner.session_ttl
    }

    pub fn query_timeout(&self) -> Duration {
        self.inner.query_timeout
    }

    pub fn uptime_secs(&self) -> u64 {
        self.inner.start_time.elapsed().as_secs()
    }

    #[cfg(feature = "auth")]
    pub fn auth(&self) -> Option<&auth::AuthProvider> {
        self.inner.auth.as_ref()
    }

    #[cfg(feature = "auth")]
    pub fn has_auth(&self) -> bool {
        self.inner.auth.as_ref().is_some_and(|a| a.is_enabled())
    }

    // --- Maintenance ---

    /// Clean up expired sessions. Returns count removed.
    pub fn cleanup_expired_sessions(&self) -> usize {
        self.inner.sessions.cleanup_expired(self.inner.session_ttl)
    }

    /// Clean up rate limiter entries.
    pub fn cleanup_rate_limits(&self) {
        self.inner.rate_limiter.cleanup();
    }

    // --- Convenience: effective timeout ---

    /// Computes the effective timeout for a query, considering per-request
    /// override and global default.
    pub fn effective_timeout(&self, req_timeout_ms: Option<u64>) -> Option<Duration> {
        match req_timeout_ms {
            Some(0) => None,
            Some(ms) => Some(Duration::from_millis(ms)),
            None => {
                let global = self.query_timeout();
                if global.is_zero() {
                    None
                } else {
                    Some(global)
                }
            }
        }
    }
}

/// Resolve a database name from an optional request field.
pub fn resolve_db_name(database: Option<&str>) -> &str {
    database.unwrap_or("default")
}
