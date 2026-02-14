//! Application state: wraps `ServiceState` with HTTP-specific fields.
//!
//! `AppState` provides transparent access to all `ServiceState` methods
//! via `Deref`, and adds transport-specific config like CORS origins.

use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use grafeo_service::{ServiceConfig, ServiceState};

use crate::config::Config;

/// Shared application state, cloneable across handlers.
///
/// Wraps `ServiceState` (business logic) and adds transport-specific fields.
/// All `ServiceState` methods are available directly via `Deref`.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppInner>,
}

struct AppInner {
    service: ServiceState,
    cors_origins: Vec<String>,
}

impl Deref for AppState {
    type Target = ServiceState;

    fn deref(&self) -> &ServiceState {
        &self.inner.service
    }
}

impl AppState {
    /// Creates a new application state from config.
    pub fn new(config: &Config) -> Self {
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

        Self {
            inner: Arc::new(AppInner {
                service: ServiceState::new(&service_config),
                cors_origins: config.cors_origins.clone(),
            }),
        }
    }

    /// Creates an in-memory application state (for tests and ephemeral use).
    pub fn new_in_memory(session_ttl: u64) -> Self {
        Self {
            inner: Arc::new(AppInner {
                service: ServiceState::new_in_memory(session_ttl),
                cors_origins: vec![],
            }),
        }
    }

    /// Creates an in-memory state with token authentication enabled (for tests).
    #[cfg(feature = "auth")]
    pub fn new_in_memory_with_auth(session_ttl: u64, auth_token: String) -> Self {
        Self {
            inner: Arc::new(AppInner {
                service: ServiceState::new_in_memory_with_auth(session_ttl, auth_token),
                cors_origins: vec![],
            }),
        }
    }

    /// Creates an in-memory state with basic auth enabled (for tests).
    #[cfg(feature = "auth")]
    pub fn new_in_memory_with_basic_auth(session_ttl: u64, user: String, password: String) -> Self {
        Self {
            inner: Arc::new(AppInner {
                service: ServiceState::new_in_memory_with_basic_auth(session_ttl, user, password),
                cors_origins: vec![],
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
            inner: Arc::new(AppInner {
                service: ServiceState::new_in_memory_with_rate_limit(
                    session_ttl,
                    max_requests,
                    window,
                ),
                cors_origins: vec![],
            }),
        }
    }

    /// Returns the configured CORS allowed origins.
    pub fn cors_origins(&self) -> &[String] {
        &self.inner.cors_origins
    }

    /// Returns a reference to the underlying service state.
    pub fn service(&self) -> &ServiceState {
        &self.inner.service
    }
}
