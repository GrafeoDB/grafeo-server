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

pub mod admin;
pub mod auth;
pub mod backup;
#[cfg(feature = "push-changefeed")]
pub mod changefeed;
#[cfg(feature = "sync")]
pub mod crdt;
pub mod database;
pub mod error;
pub mod metrics;
pub mod query;
pub mod rate_limit;
#[cfg(feature = "replication")]
pub mod replication;
pub mod schema;
pub mod search;
pub mod session;
pub mod stream;
#[cfg(feature = "sync")]
pub mod sync;
#[cfg(feature = "auth")]
pub mod token_service;
#[cfg(feature = "auth")]
pub mod token_store;
pub mod types;

use std::path::{Path, PathBuf};
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
    pub read_only: bool,
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
    #[cfg(feature = "auth")]
    pub token_store_path: Option<String>,
    #[cfg(feature = "replication")]
    pub replication_mode: replication::ReplicationMode,
    /// Directory for storing database backups.
    pub backup_dir: Option<String>,
    /// Number of backups to keep per database (retention policy).
    pub backup_retention: Option<usize>,
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
    read_only: bool,
    #[cfg(feature = "auth")]
    auth: Option<auth::AuthProvider>,
    #[cfg(feature = "push-changefeed")]
    change_hub: changefeed::ChangeHub,
    #[cfg(feature = "replication")]
    replication_mode: replication::ReplicationMode,
    #[cfg(feature = "replication")]
    replication_state: Arc<replication::ReplicationState>,
    backup_dir: Option<PathBuf>,
    backup_retention: Option<usize>,
}

impl ServiceState {
    /// Creates a new service state from config.
    pub fn new(config: &ServiceConfig) -> Self {
        #[allow(unused_mut)]
        let mut databases = DatabaseManager::new(config.data_dir.as_deref(), config.read_only);

        // Enable CDC on all databases when running as a replication primary,
        // so that mutations produce change events for replicas to consume.
        #[cfg(feature = "replication")]
        if config.replication_mode.is_primary() {
            databases.set_cdc_enabled(true);
        }

        Self {
            inner: Arc::new(Inner {
                databases,
                sessions: SessionRegistry::new(),
                metrics: Metrics::new(),
                rate_limiter: RateLimiter::new(
                    config.rate_limit,
                    Duration::from_secs(config.rate_limit_window),
                ),
                session_ttl: config.session_ttl,
                query_timeout: Duration::from_secs(config.query_timeout),
                start_time: Instant::now(),
                read_only: config.read_only,
                #[cfg(feature = "auth")]
                auth: {
                    // Resolve token store path: explicit > {data_dir}/tokens.json > None
                    // Only auto-derive from data_dir when at least one credential
                    // is configured, so data_dir alone doesn't activate auth.
                    let has_credentials = config.auth_token.is_some() || config.auth_user.is_some();
                    let store_path = config.token_store_path.clone().or_else(|| {
                        if !has_credentials {
                            return None;
                        }
                        config.data_dir.as_ref().map(|d| {
                            PathBuf::from(d)
                                .join("tokens.json")
                                .to_string_lossy()
                                .into_owned()
                        })
                    });
                    if let Some(ref path) = store_path {
                        let store = Arc::new(
                            token_store::TokenStore::load(path)
                                .unwrap_or_else(|e| panic!("failed to load token store: {e}")),
                        );
                        auth::AuthProvider::with_token_store(
                            config.auth_token.clone(),
                            config.auth_user.clone(),
                            config.auth_password.clone(),
                            store,
                        )
                    } else {
                        auth::AuthProvider::new(
                            config.auth_token.clone(),
                            config.auth_user.clone(),
                            config.auth_password.clone(),
                        )
                    }
                },
                #[cfg(feature = "push-changefeed")]
                change_hub: changefeed::ChangeHub::new(),
                #[cfg(feature = "replication")]
                replication_mode: config.replication_mode.clone(),
                #[cfg(feature = "replication")]
                replication_state: Arc::new(if let Some(ref dir) = config.data_dir {
                    replication::ReplicationState::with_persistence(std::path::PathBuf::from(dir))
                } else {
                    replication::ReplicationState::new()
                }),
                backup_dir: config.backup_dir.as_ref().map(PathBuf::from),
                backup_retention: config.backup_retention,
            }),
        }
    }

    /// Creates an in-memory service state (for tests and ephemeral use).
    pub fn new_in_memory(session_ttl: u64) -> Self {
        Self {
            inner: Arc::new(Inner {
                databases: DatabaseManager::new(None, false),
                sessions: SessionRegistry::new(),
                metrics: Metrics::new(),
                rate_limiter: RateLimiter::new(0, Duration::from_secs(60)),
                session_ttl,
                query_timeout: Duration::from_secs(30),
                start_time: Instant::now(),
                read_only: false,
                #[cfg(feature = "auth")]
                auth: None,
                #[cfg(feature = "push-changefeed")]
                change_hub: changefeed::ChangeHub::new(),
                #[cfg(feature = "replication")]
                replication_mode: replication::ReplicationMode::Standalone,
                #[cfg(feature = "replication")]
                replication_state: Arc::new(replication::ReplicationState::new()),
                backup_dir: None,
                backup_retention: None,
            }),
        }
    }

    /// Creates an in-memory state with token authentication enabled (for tests).
    #[cfg(feature = "auth")]
    pub fn new_in_memory_with_auth(session_ttl: u64, auth_token: String) -> Self {
        Self {
            inner: Arc::new(Inner {
                databases: DatabaseManager::new(None, false),
                sessions: SessionRegistry::new(),
                metrics: Metrics::new(),
                rate_limiter: RateLimiter::new(0, Duration::from_secs(60)),
                session_ttl,
                query_timeout: Duration::from_secs(30),
                start_time: Instant::now(),
                read_only: false,
                auth: auth::AuthProvider::new(Some(auth_token), None, None),
                #[cfg(feature = "push-changefeed")]
                change_hub: changefeed::ChangeHub::new(),
                #[cfg(feature = "replication")]
                replication_mode: replication::ReplicationMode::Standalone,
                #[cfg(feature = "replication")]
                replication_state: Arc::new(replication::ReplicationState::new()),
                backup_dir: None,
                backup_retention: None,
            }),
        }
    }

    /// Creates an in-memory state with basic auth enabled (for tests).
    #[cfg(feature = "auth")]
    pub fn new_in_memory_with_basic_auth(session_ttl: u64, user: String, password: String) -> Self {
        Self {
            inner: Arc::new(Inner {
                databases: DatabaseManager::new(None, false),
                sessions: SessionRegistry::new(),
                metrics: Metrics::new(),
                rate_limiter: RateLimiter::new(0, Duration::from_secs(60)),
                session_ttl,
                query_timeout: Duration::from_secs(30),
                start_time: Instant::now(),
                read_only: false,
                auth: auth::AuthProvider::new(None, Some(user), Some(password)),
                #[cfg(feature = "push-changefeed")]
                change_hub: changefeed::ChangeHub::new(),
                #[cfg(feature = "replication")]
                replication_mode: replication::ReplicationMode::Standalone,
                #[cfg(feature = "replication")]
                replication_state: Arc::new(replication::ReplicationState::new()),
                backup_dir: None,
                backup_retention: None,
            }),
        }
    }

    /// Creates an in-memory state with read-only mode enabled (for tests).
    pub fn new_in_memory_read_only(session_ttl: u64) -> Self {
        Self {
            inner: Arc::new(Inner {
                databases: DatabaseManager::new(None, true),
                sessions: SessionRegistry::new(),
                metrics: Metrics::new(),
                rate_limiter: RateLimiter::new(0, Duration::from_secs(60)),
                session_ttl,
                query_timeout: Duration::from_secs(30),
                start_time: Instant::now(),
                read_only: true,
                #[cfg(feature = "auth")]
                auth: None,
                #[cfg(feature = "push-changefeed")]
                change_hub: changefeed::ChangeHub::new(),
                #[cfg(feature = "replication")]
                replication_mode: replication::ReplicationMode::Standalone,
                #[cfg(feature = "replication")]
                replication_state: Arc::new(replication::ReplicationState::new()),
                backup_dir: None,
                backup_retention: None,
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
                databases: DatabaseManager::new(None, false),
                sessions: SessionRegistry::new(),
                metrics: Metrics::new(),
                rate_limiter: RateLimiter::new(max_requests, window),
                session_ttl,
                query_timeout: Duration::from_secs(30),
                start_time: Instant::now(),
                read_only: false,
                #[cfg(feature = "auth")]
                auth: None,
                #[cfg(feature = "push-changefeed")]
                change_hub: changefeed::ChangeHub::new(),
                #[cfg(feature = "replication")]
                replication_mode: replication::ReplicationMode::Standalone,
                #[cfg(feature = "replication")]
                replication_state: Arc::new(replication::ReplicationState::new()),
                backup_dir: None,
                backup_retention: None,
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

    pub fn is_read_only(&self) -> bool {
        self.inner.read_only
    }

    pub fn uptime_secs(&self) -> u64 {
        self.inner.start_time.elapsed().as_secs()
    }

    #[cfg(feature = "push-changefeed")]
    pub fn change_hub(&self) -> &changefeed::ChangeHub {
        &self.inner.change_hub
    }

    #[cfg(feature = "auth")]
    pub fn auth(&self) -> Option<&auth::AuthProvider> {
        self.inner.auth.as_ref()
    }

    /// Returns `true` if this instance is a read-only replica.
    #[cfg(feature = "replication")]
    pub fn is_replica(&self) -> bool {
        self.inner.replication_mode.is_replica()
    }

    /// Returns `true` if this instance is a read-only replica.
    #[cfg(not(feature = "replication"))]
    pub fn is_replica(&self) -> bool {
        false
    }

    /// Returns `true` if client-facing queries should be read-only.
    ///
    /// True when the server is in replica mode or global read-only mode.
    pub fn is_query_read_only(&self) -> bool {
        self.inner.read_only || self.is_replica()
    }

    /// Returns the replication mode for this instance.
    #[cfg(feature = "replication")]
    pub fn replication_mode(&self) -> &replication::ReplicationMode {
        &self.inner.replication_mode
    }

    /// Returns the shared replication state (epoch tracking, errors).
    #[cfg(feature = "replication")]
    pub fn replication_state(&self) -> &Arc<replication::ReplicationState> {
        &self.inner.replication_state
    }

    #[cfg(feature = "auth")]
    pub fn has_auth(&self) -> bool {
        self.inner.auth.as_ref().is_some_and(|a| a.is_enabled())
    }

    // --- Backup ---

    /// Returns the configured backup directory, if any.
    pub fn backup_dir(&self) -> Option<&Path> {
        self.inner.backup_dir.as_deref()
    }

    /// Returns the backup retention count, if configured.
    pub fn backup_retention(&self) -> Option<usize> {
        self.inner.backup_retention
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
                if global.is_zero() { None } else { Some(global) }
            }
        }
    }
}

/// Resolve a database name from an optional request field.
pub fn resolve_db_name(database: Option<&str>) -> &str {
    database.unwrap_or("default")
}
