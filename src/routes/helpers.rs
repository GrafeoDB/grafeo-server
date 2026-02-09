//! Shared helper functions for route handlers.

use std::time::Duration;

use crate::error::ApiError;
use crate::metrics::Language;
use crate::state::AppState;

use super::types::QueryResponse;

/// Resolves the database name from the request, defaulting to "default".
pub fn resolve_db_name(database: Option<&String>) -> &str {
    database.map_or("default", |s| s.as_str())
}

/// Computes the effective timeout for a query.
pub fn effective_timeout(state: &AppState, req_timeout_ms: Option<u64>) -> Option<Duration> {
    match req_timeout_ms {
        Some(0) => None,
        Some(ms) => Some(Duration::from_millis(ms)),
        None => {
            let global = state.query_timeout();
            if global.is_zero() { None } else { Some(global) }
        }
    }
}

/// Runs a blocking task with an optional timeout.
pub async fn run_with_timeout<F, T>(timeout: Option<Duration>, task: F) -> Result<T, ApiError>
where
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
    T: Send + 'static,
{
    let handle = tokio::task::spawn_blocking(task);
    if let Some(dur) = timeout {
        match tokio::time::timeout(dur, handle).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => Err(ApiError::Internal(e.to_string())),
            Err(_) => {
                tracing::warn!("query timed out after {dur:?}");
                Err(ApiError::Timeout)
            }
        }
    } else {
        handle
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
    }
}

/// Records query metrics after execution.
pub fn record_metrics(state: &AppState, lang: Language, result: &Result<QueryResponse, ApiError>) {
    match result {
        Ok(resp) => {
            let dur_us = resp.execution_time_ms.map_or(0, |ms| (ms * 1000.0) as u64);
            state.metrics().record_query(lang, dur_us);
        }
        Err(_) => {
            state.metrics().record_query_error(lang);
        }
    }
}
