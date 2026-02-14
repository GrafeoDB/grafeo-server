//! Centralized query execution service.
//!
//! Extracted from `routes/query.rs`, `routes/helpers.rs`, and `routes/batch.rs`.
//! All transports (HTTP, GWP, Bolt) call through here instead of touching
//! the engine directly. This ensures consistent language dispatch, timeout
//! handling, and metrics recording across all protocols.

use std::collections::HashMap;
use std::time::Duration;

use grafeo_engine::database::QueryResult;

use crate::database::DatabaseManager;
use crate::error::ServiceError;
use crate::metrics::{Language, Metrics, determine_language};
use crate::session::{ManagedSession, SessionRegistry};
use crate::types::BatchQuery;

/// Centralized query execution service.
///
/// Stateless method collection — all state is borrowed from `ServiceState`.
pub struct QueryService;

impl QueryService {
    /// Auto-commit query execution.
    ///
    /// Creates a fresh session, dispatches by language, runs with timeout,
    /// records metrics, and returns raw `QueryResult`. Transport crates
    /// handle value encoding (JSON, GWP, PackStream).
    pub async fn execute(
        databases: &DatabaseManager,
        metrics: &Metrics,
        db_name: &str,
        statement: &str,
        language: Option<&str>,
        params: Option<HashMap<String, grafeo_common::Value>>,
        timeout: Option<Duration>,
    ) -> Result<QueryResult, ServiceError> {
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        let lang = determine_language(language);
        let stmt = statement.to_owned();

        let result = run_with_timeout(timeout, move || {
            let session = entry.db.session();
            dispatch_query(&session, &stmt, lang, params.as_ref())
        })
        .await;

        match &result {
            Ok(qr) => {
                let dur_us = qr.execution_time_ms.map_or(0, |ms| (ms * 1000.0) as u64);
                metrics.record_query(lang, dur_us);
            }
            Err(_) => {
                metrics.record_query_error(lang);
            }
        }

        result
    }

    /// Execute a query within an existing transaction session.
    #[allow(clippy::too_many_arguments)]
    pub async fn tx_execute(
        sessions: &SessionRegistry,
        metrics: &Metrics,
        session_id: &str,
        ttl_secs: u64,
        statement: &str,
        language: Option<&str>,
        params: Option<HashMap<String, grafeo_common::Value>>,
        timeout: Option<Duration>,
    ) -> Result<QueryResult, ServiceError> {
        let session_arc = sessions
            .get(session_id, ttl_secs)
            .ok_or(ServiceError::SessionNotFound)?;

        let lang = determine_language(language);
        let stmt = statement.to_owned();

        let result = run_with_timeout(timeout, move || {
            let session = session_arc.lock();
            dispatch_query(&session.engine_session, &stmt, lang, params.as_ref())
        })
        .await;

        match &result {
            Ok(qr) => {
                let dur_us = qr.execution_time_ms.map_or(0, |ms| (ms * 1000.0) as u64);
                metrics.record_query(lang, dur_us);
            }
            Err(_) => {
                metrics.record_query_error(lang);
            }
        }

        result
    }

    /// Begin a new transaction. Returns session ID.
    pub async fn begin_tx(
        databases: &DatabaseManager,
        sessions: &SessionRegistry,
        db_name: &str,
    ) -> Result<String, ServiceError> {
        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        let db_name = db_name.to_owned();
        let session_id = tokio::task::spawn_blocking(move || {
            let mut engine_session = entry.db.session();
            engine_session
                .begin_tx()
                .map_err(|e| ServiceError::Internal(e.to_string()))?;
            Ok::<_, ServiceError>(engine_session)
        })
        .await
        .map_err(|e| ServiceError::Internal(e.to_string()))??;

        let id = sessions.create(session_id, &db_name);
        Ok(id)
    }

    /// Commit a transaction.
    pub async fn commit(
        sessions: &SessionRegistry,
        session_id: &str,
        ttl_secs: u64,
    ) -> Result<(), ServiceError> {
        let session_arc = sessions
            .get(session_id, ttl_secs)
            .ok_or(ServiceError::SessionNotFound)?;

        tokio::task::spawn_blocking(move || {
            let mut session = session_arc.lock();
            session
                .engine_session
                .commit()
                .map_err(|e| ServiceError::Internal(e.to_string()))
        })
        .await
        .map_err(|e| ServiceError::Internal(e.to_string()))??;

        sessions.remove(session_id);
        Ok(())
    }

    /// Rollback a transaction.
    pub async fn rollback(
        sessions: &SessionRegistry,
        session_id: &str,
        ttl_secs: u64,
    ) -> Result<(), ServiceError> {
        let session_arc = sessions
            .get(session_id, ttl_secs)
            .ok_or(ServiceError::SessionNotFound)?;

        tokio::task::spawn_blocking(move || {
            let mut session = session_arc.lock();
            session
                .engine_session
                .rollback()
                .map_err(|e| ServiceError::Internal(e.to_string()))
        })
        .await
        .map_err(|e| ServiceError::Internal(e.to_string()))??;

        sessions.remove(session_id);
        Ok(())
    }

    /// Batch execute — all queries in one implicit transaction.
    /// Rolls back on first failure.
    pub async fn batch_execute(
        databases: &DatabaseManager,
        metrics: &Metrics,
        db_name: &str,
        queries: Vec<BatchQuery>,
        timeout: Option<Duration>,
    ) -> Result<Vec<QueryResult>, ServiceError> {
        if queries.is_empty() {
            return Ok(vec![]);
        }

        let entry = databases
            .get(db_name)
            .ok_or_else(|| ServiceError::NotFound(format!("database '{db_name}' not found")))?;

        // Collect language info for post-execution metrics recording.
        // Metrics use atomics (not Clone), so we record after the blocking task.
        let languages: Vec<Language> = queries
            .iter()
            .map(|q| determine_language(q.language.as_deref()))
            .collect();

        let results = run_with_timeout(timeout, move || {
            let mut session = entry.db.session();
            session
                .begin_tx()
                .map_err(|e| ServiceError::Internal(e.to_string()))?;

            let mut results: Vec<QueryResult> = Vec::with_capacity(queries.len());

            for (idx, item) in queries.iter().enumerate() {
                let lang = determine_language(item.language.as_deref());
                match dispatch_query(&session, &item.statement, lang, item.params.as_ref()) {
                    Ok(qr) => results.push(qr),
                    Err(e) => {
                        let _ = session.rollback();
                        return Err(ServiceError::BadRequest(format!(
                            "query at index {idx} failed: {e}"
                        )));
                    }
                }
            }

            session
                .commit()
                .map_err(|e| ServiceError::Internal(e.to_string()))?;

            Ok(results)
        })
        .await?;

        // Record metrics for all successful queries
        for (lang, qr) in languages.iter().zip(results.iter()) {
            let dur_us = qr.execution_time_ms.map_or(0, |ms| (ms * 1000.0) as u64);
            metrics.record_query(*lang, dur_us);
        }

        Ok(results)
    }

    /// Provides direct access to a session Arc for transport-specific use
    /// (e.g., GWP needs to hold session state across multiple calls).
    pub fn get_session(
        sessions: &SessionRegistry,
        session_id: &str,
        ttl_secs: u64,
    ) -> Result<std::sync::Arc<parking_lot::Mutex<ManagedSession>>, ServiceError> {
        sessions
            .get(session_id, ttl_secs)
            .ok_or(ServiceError::SessionNotFound)
    }
}

// ---------------------------------------------------------------------------
// Language dispatch
// ---------------------------------------------------------------------------

/// Dispatch a query to the appropriate engine method based on language.
fn dispatch_query(
    session: &grafeo_engine::Session,
    statement: &str,
    language: Language,
    params: Option<&HashMap<String, grafeo_common::Value>>,
) -> Result<QueryResult, ServiceError> {
    let result = match (language, params) {
        // GQL (default)
        (Language::Gql, Some(p)) => session.execute_with_params(statement, p.clone()),
        (Language::Gql, None) => session.execute(statement),

        // Cypher
        #[cfg(feature = "cypher")]
        (Language::Cypher, _) => session.execute_cypher(statement),
        #[cfg(not(feature = "cypher"))]
        (Language::Cypher, _) => {
            return Err(ServiceError::BadRequest(
                "cypher support not enabled in this build".to_string(),
            ));
        }

        // GraphQL
        #[cfg(feature = "graphql")]
        (Language::Graphql, Some(p)) => session.execute_graphql_with_params(statement, p.clone()),
        #[cfg(feature = "graphql")]
        (Language::Graphql, None) => session.execute_graphql(statement),
        #[cfg(not(feature = "graphql"))]
        (Language::Graphql, _) => {
            return Err(ServiceError::BadRequest(
                "graphql support not enabled in this build".to_string(),
            ));
        }

        // Gremlin
        #[cfg(feature = "gremlin")]
        (Language::Gremlin, Some(p)) => session.execute_gremlin_with_params(statement, p.clone()),
        #[cfg(feature = "gremlin")]
        (Language::Gremlin, None) => session.execute_gremlin(statement),
        #[cfg(not(feature = "gremlin"))]
        (Language::Gremlin, _) => {
            return Err(ServiceError::BadRequest(
                "gremlin support not enabled in this build".to_string(),
            ));
        }

        // SPARQL
        #[cfg(feature = "sparql")]
        (Language::Sparql, _) => session.execute_sparql(statement),
        #[cfg(not(feature = "sparql"))]
        (Language::Sparql, _) => {
            return Err(ServiceError::BadRequest(
                "sparql support not enabled in this build".to_string(),
            ));
        }

        // SQL/PGQ
        #[cfg(feature = "sql-pgq")]
        (Language::SqlPgq, Some(p)) => session.execute_sql_with_params(statement, p.clone()),
        #[cfg(feature = "sql-pgq")]
        (Language::SqlPgq, None) => session.execute_sql(statement),
        #[cfg(not(feature = "sql-pgq"))]
        (Language::SqlPgq, _) => {
            return Err(ServiceError::BadRequest(
                "sql-pgq support not enabled in this build".to_string(),
            ));
        }
    };

    result.map_err(|e| ServiceError::BadRequest(e.to_string()))
}

// ---------------------------------------------------------------------------
// Timeout + spawn_blocking
// ---------------------------------------------------------------------------

/// Run a blocking operation with optional timeout.
///
/// Uses `tokio::task::spawn_blocking` to avoid blocking the async runtime.
pub async fn run_with_timeout<F, T>(timeout: Option<Duration>, task: F) -> Result<T, ServiceError>
where
    F: FnOnce() -> Result<T, ServiceError> + Send + 'static,
    T: Send + 'static,
{
    let handle = tokio::task::spawn_blocking(task);

    if let Some(dur) = timeout
        && !dur.is_zero()
    {
        match tokio::time::timeout(dur, handle).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => Err(ServiceError::Internal(e.to_string())),
            Err(_) => {
                tracing::warn!("query timed out after {dur:?}");
                Err(ServiceError::Timeout)
            }
        }
    } else {
        handle
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?
    }
}
