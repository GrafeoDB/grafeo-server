//! HTTP application state: wraps `ServiceState` with HTTP-specific fields.
//!
//! `AppState` provides transparent access to all `ServiceState` methods
//! via `Deref`, and adds transport-specific config like CORS origins and
//! compiled feature flags.

use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use grafeo_service::ServiceState;
use grafeo_service::types::EnabledFeatures;

/// Shared HTTP application state, cloneable across handlers.
///
/// Wraps `ServiceState` (business logic) and adds HTTP-specific fields.
/// All `ServiceState` methods are available directly via `Deref`.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppInner>,
}

struct AppInner {
    service: ServiceState,
    cors_origins: Vec<String>,
    enabled_features: EnabledFeatures,
}

impl Deref for AppState {
    type Target = ServiceState;

    fn deref(&self) -> &ServiceState {
        &self.inner.service
    }
}

impl AppState {
    /// Creates a new HTTP application state.
    pub fn new(
        service: ServiceState,
        cors_origins: Vec<String>,
        enabled_features: EnabledFeatures,
    ) -> Self {
        Self {
            inner: Arc::new(AppInner {
                service,
                cors_origins,
                enabled_features,
            }),
        }
    }

    /// Creates an in-memory application state (for tests and ephemeral use).
    pub fn new_in_memory(session_ttl: u64) -> Self {
        Self {
            inner: Arc::new(AppInner {
                service: ServiceState::new_in_memory(session_ttl),
                cors_origins: vec![],
                enabled_features: EnabledFeatures::default(),
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
                enabled_features: EnabledFeatures::default(),
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
                enabled_features: EnabledFeatures::default(),
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
                enabled_features: EnabledFeatures::default(),
            }),
        }
    }

    /// Returns the configured CORS allowed origins.
    pub fn cors_origins(&self) -> &[String] {
        &self.inner.cors_origins
    }

    /// Returns the compiled feature flags.
    pub fn enabled_features(&self) -> &EnabledFeatures {
        &self.inner.enabled_features
    }

    /// Returns a reference to the underlying service state.
    pub fn service(&self) -> &ServiceState {
        &self.inner.service
    }
}
