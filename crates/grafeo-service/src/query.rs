//! Centralized query execution service.
//!
//! Extracted from `routes/query.rs`, `routes/helpers.rs`, and `routes/batch.rs`.
//! All transports (HTTP, GWP, Bolt) call through here instead of touching
//! the engine directly. This ensures consistent language dispatch, timeout
//! handling, and metrics recording across all protocols.

use std::collections::HashMap;
use std::time::Duration;

use grafeo_engine::auth::{Identity, Role};
use grafeo_engine::database::QueryResult;

use crate::database::DatabaseManager;
use crate::error::ServiceError;
use crate::metrics::{Language, Metrics, determine_language};
use crate::session::{ManagedSession, SessionRegistry};
use crate::types::BatchQuery;

/// Create a session from a database handle, using the provided identity
/// or falling back to read_only / full-access based on the flag.
fn create_session(
    db: &grafeo_engine::GrafeoDB,
    identity: Option<Identity>,
    read_only: bool,
) -> grafeo_engine::Session {
    match identity {
        Some(id) => db.session_with_identity(id),
        None if read_only => db.session_with_role(Role::ReadOnly),
        None => db.session(),
    }
}

/// Centralized query execution service.
///
/// Stateless method collection: all state is borrowed from `ServiceState`.
pub struct QueryService;

impl QueryService {
    /// Auto-commit query execution.
    ///
    /// Creates a fresh session, dispatches by language, runs with timeout,
    /// records metrics, and returns raw `QueryResult`. Transport crates
    /// handle value encoding (JSON, GWP, PackStream).
    #[allow(clippy::too_many_arguments)]
    pub async fn execute(
        databases: &DatabaseManager,
        metrics: &Metrics,
        db_name: &str,
        statement: &str,
        language: Option<&str>,
        params: Option<HashMap<String, grafeo_common::Value>>,
        timeout: Option<Duration>,
        read_only: bool,
        identity: Option<Identity>,
    ) -> Result<QueryResult, ServiceError> {
        let entry = databases.get_available(db_name)?;

        let lang = determine_language(language);
        let stmt = statement.to_owned();

        let result = run_with_timeout(timeout, move || {
            let db = entry.db();
            let session = create_session(&db, identity, read_only);
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
        caller_token_id: Option<&str>,
    ) -> Result<QueryResult, ServiceError> {
        let session_arc = sessions
            .get(session_id, ttl_secs, caller_token_id)
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
    ///
    /// `owner_token_id` ties the session to the authenticated token so that
    /// only the same token can query/commit/rollback this transaction.
    pub async fn begin_tx(
        databases: &DatabaseManager,
        sessions: &SessionRegistry,
        db_name: &str,
        read_only: bool,
        identity: Option<Identity>,
        owner_token_id: Option<String>,
    ) -> Result<String, ServiceError> {
        let entry = databases.get_available(db_name)?;

        let db_name = db_name.to_owned();
        let session_id = tokio::task::spawn_blocking(move || {
            let db = entry.db();
            let mut engine_session = create_session(&db, identity, read_only);
            engine_session
                .begin_transaction()
                .map_err(|e| ServiceError::Internal(e.to_string()))?;
            Ok::<_, ServiceError>(engine_session)
        })
        .await
        .map_err(|e| ServiceError::Internal(e.to_string()))??;

        let id = sessions.create(session_id, &db_name, owner_token_id);
        Ok(id)
    }

    /// Commit a transaction.
    pub async fn commit(
        sessions: &SessionRegistry,
        session_id: &str,
        ttl_secs: u64,
        caller_token_id: Option<&str>,
    ) -> Result<(), ServiceError> {
        let session_arc = sessions
            .get(session_id, ttl_secs, caller_token_id)
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
        caller_token_id: Option<&str>,
    ) -> Result<(), ServiceError> {
        let session_arc = sessions
            .get(session_id, ttl_secs, caller_token_id)
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

    /// Batch execute: all queries in one implicit transaction.
    /// Rolls back on first failure.
    pub async fn batch_execute(
        databases: &DatabaseManager,
        metrics: &Metrics,
        db_name: &str,
        queries: Vec<BatchQuery>,
        timeout: Option<Duration>,
        read_only: bool,
        identity: Option<Identity>,
    ) -> Result<Vec<QueryResult>, ServiceError> {
        if queries.is_empty() {
            return Ok(vec![]);
        }

        let entry = databases.get_available(db_name)?;

        // Collect language info for post-execution metrics recording.
        // Metrics use atomics (not Clone), so we record after the blocking task.
        let languages: Vec<Language> = queries
            .iter()
            .map(|q| determine_language(q.language.as_deref()))
            .collect();

        let results = run_with_timeout(timeout, move || {
            let db = entry.db();
            let mut session = create_session(&db, identity, read_only);
            session
                .begin_transaction()
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

    /// Dispatch a query on an existing engine session by language string.
    ///
    /// For transport crates (Bolt, GWP) that manage their own engine sessions.
    /// Handles language dispatch and feature-gate validation.
    pub fn dispatch(
        session: &grafeo_engine::Session,
        statement: &str,
        language: Option<&str>,
        params: Option<&HashMap<String, grafeo_common::Value>>,
    ) -> Result<QueryResult, ServiceError> {
        let lang = determine_language(language);
        dispatch_query(session, statement, lang, params)
    }

    /// Provides direct access to a session Arc for transport-specific use
    /// (e.g., GWP needs to hold session state across multiple calls).
    pub fn get_session(
        sessions: &SessionRegistry,
        session_id: &str,
        ttl_secs: u64,
    ) -> Result<std::sync::Arc<parking_lot::Mutex<ManagedSession>>, ServiceError> {
        sessions
            .get(session_id, ttl_secs, None)
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

    // The engine wraps PermissionDenied as Error::Query(Semantic, ...) with no
    // distinct error kind, so string matching is the only way to distinguish
    // permission errors from other semantic errors (type mismatches, unknown
    // identifiers, etc.). Track: GrafeoDB/grafeo#TBD for a dedicated
    // QueryErrorKind::PermissionDenied variant.
    result.map_err(|e| {
        let msg = e.to_string();
        if msg.contains("permission denied") {
            ServiceError::Forbidden(msg)
        } else {
            ServiceError::BadRequest(msg)
        }
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ServiceState;

    fn state() -> ServiceState {
        ServiceState::new_in_memory(300)
    }

    // -----------------------------------------------------------------------
    // execute: auto-commit queries
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn execute_simple_query() {
        let s = state();
        let qr = QueryService::execute(
            s.databases(),
            s.metrics(),
            "default",
            "MATCH (n) RETURN n LIMIT 1",
            None,
            None,
            None,
            false,
            None,
        )
        .await
        .unwrap();
        assert!(!qr.columns.is_empty());
    }

    #[tokio::test]
    async fn execute_not_found_database() {
        let s = state();
        let err = QueryService::execute(
            s.databases(),
            s.metrics(),
            "nonexistent",
            "MATCH (n) RETURN n",
            None,
            None,
            None,
            false,
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[tokio::test]
    async fn execute_syntax_error() {
        let s = state();
        let err = QueryService::execute(
            s.databases(),
            s.metrics(),
            "default",
            "THIS IS NOT VALID GQL",
            None,
            None,
            None,
            false,
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ServiceError::BadRequest(_)));
    }

    #[tokio::test]
    async fn execute_with_params() {
        let s = state();
        let mut params = HashMap::new();
        params.insert("x".to_string(), grafeo_common::Value::Int64(42));
        let qr = QueryService::execute(
            s.databases(),
            s.metrics(),
            "default",
            "RETURN $x AS val",
            None,
            Some(params),
            None,
            false,
            None,
        )
        .await
        .unwrap();
        assert_eq!(qr.columns, vec!["val"]);
        assert_eq!(qr.rows().len(), 1);
    }

    #[tokio::test]
    async fn execute_with_language_dispatch() {
        let s = state();
        // GQL is always available
        let qr = QueryService::execute(
            s.databases(),
            s.metrics(),
            "default",
            "MATCH (n) RETURN n LIMIT 0",
            Some("gql"),
            None,
            None,
            false,
            None,
        )
        .await
        .unwrap();
        assert!(qr.rows().is_empty());
    }

    #[tokio::test]
    async fn execute_records_success_metrics() {
        let s = state();
        QueryService::execute(
            s.databases(),
            s.metrics(),
            "default",
            "MATCH (n) RETURN n LIMIT 0",
            None,
            None,
            None,
            false,
            None,
        )
        .await
        .unwrap();
        let rendered = s.metrics().render(0, 0, 0, 0, 0, None);
        assert!(rendered.contains("grafeo_queries_total{language=\"gql\"} 1"));
    }

    #[tokio::test]
    async fn execute_records_error_metrics() {
        let s = state();
        let _ = QueryService::execute(
            s.databases(),
            s.metrics(),
            "default",
            "THIS IS NOT VALID",
            None,
            None,
            None,
            false,
            None,
        )
        .await;
        let rendered = s.metrics().render(0, 0, 0, 0, 0, None);
        assert!(rendered.contains("grafeo_query_errors_total{language=\"gql\"} 1"));
    }

    #[tokio::test]
    async fn execute_with_timeout() {
        let s = state();
        // A query that completes instantly should succeed even with a short timeout.
        let qr = QueryService::execute(
            s.databases(),
            s.metrics(),
            "default",
            "MATCH (n) RETURN n LIMIT 0",
            None,
            None,
            Some(Duration::from_secs(10)),
            false,
            None,
        )
        .await
        .unwrap();
        assert!(qr.rows().is_empty());
    }

    // -----------------------------------------------------------------------
    // begin_tx / tx_execute / commit / rollback
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn begin_tx_returns_session_id() {
        let s = state();
        let id = QueryService::begin_tx(s.databases(), s.sessions(), "default", false, None, None)
            .await
            .unwrap();
        assert!(!id.is_empty());
    }

    #[tokio::test]
    async fn begin_tx_not_found_database() {
        let s = state();
        let err =
            QueryService::begin_tx(s.databases(), s.sessions(), "no_such_db", false, None, None)
                .await
                .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[tokio::test]
    async fn tx_execute_then_commit() {
        let s = state();
        let id = QueryService::begin_tx(s.databases(), s.sessions(), "default", false, None, None)
            .await
            .unwrap();

        let qr = QueryService::tx_execute(
            s.sessions(),
            s.metrics(),
            &id,
            300,
            "CREATE (n:Person {name: 'Alice'}) RETURN n.name AS name",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(qr.rows().len(), 1);

        QueryService::commit(s.sessions(), &id, 300, None)
            .await
            .unwrap();

        // Session should be removed after commit.
        let err = QueryService::commit(s.sessions(), &id, 300, None)
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::SessionNotFound));
    }

    #[tokio::test]
    async fn tx_execute_then_rollback() {
        let s = state();
        let id = QueryService::begin_tx(s.databases(), s.sessions(), "default", false, None, None)
            .await
            .unwrap();

        QueryService::tx_execute(
            s.sessions(),
            s.metrics(),
            &id,
            300,
            "CREATE (n:Temp {val: 1})",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        QueryService::rollback(s.sessions(), &id, 300, None)
            .await
            .unwrap();

        // Session should be removed after rollback.
        let err = QueryService::rollback(s.sessions(), &id, 300, None)
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::SessionNotFound));
    }

    #[tokio::test]
    async fn tx_execute_session_not_found() {
        let s = state();
        let err = QueryService::tx_execute(
            s.sessions(),
            s.metrics(),
            "nonexistent-session",
            300,
            "MATCH (n) RETURN n",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ServiceError::SessionNotFound));
    }

    #[tokio::test]
    async fn commit_session_not_found() {
        let s = state();
        let err = QueryService::commit(s.sessions(), "bad-id", 300, None)
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::SessionNotFound));
    }

    #[tokio::test]
    async fn rollback_session_not_found() {
        let s = state();
        let err = QueryService::rollback(s.sessions(), "bad-id", 300, None)
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::SessionNotFound));
    }

    // -----------------------------------------------------------------------
    // batch_execute
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn batch_empty_returns_empty() {
        let s = state();
        let results = QueryService::batch_execute(
            s.databases(),
            s.metrics(),
            "default",
            vec![],
            None,
            false,
            None,
        )
        .await
        .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn batch_single_query() {
        let s = state();
        let results = QueryService::batch_execute(
            s.databases(),
            s.metrics(),
            "default",
            vec![BatchQuery {
                statement: "MATCH (n) RETURN n LIMIT 0".to_string(),
                language: None,
                params: None,
            }],
            None,
            false,
            None,
        )
        .await
        .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn batch_multiple_queries() {
        let s = state();
        let results = QueryService::batch_execute(
            s.databases(),
            s.metrics(),
            "default",
            vec![
                BatchQuery {
                    statement: "CREATE (a:A {val: 1})".to_string(),
                    language: None,
                    params: None,
                },
                BatchQuery {
                    statement: "CREATE (b:B {val: 2})".to_string(),
                    language: None,
                    params: None,
                },
                BatchQuery {
                    statement: "MATCH (n) RETURN n.val AS v".to_string(),
                    language: None,
                    params: None,
                },
            ],
            None,
            false,
            None,
        )
        .await
        .unwrap();
        assert_eq!(results.len(), 3);
        // The third query should see both nodes created in the same transaction.
        assert_eq!(results[2].rows().len(), 2);
    }

    #[tokio::test]
    async fn batch_partial_failure_reports_index() {
        let s = state();
        let err = QueryService::batch_execute(
            s.databases(),
            s.metrics(),
            "default",
            vec![
                BatchQuery {
                    statement: "CREATE (a:A {val: 1})".to_string(),
                    language: None,
                    params: None,
                },
                BatchQuery {
                    statement: "NOT VALID GQL AT ALL".to_string(),
                    language: None,
                    params: None,
                },
            ],
            None,
            false,
            None,
        )
        .await
        .unwrap_err();
        // Error message should reference the failing index.
        let msg = err.to_string();
        assert!(msg.contains("index 1"), "expected index in error: {msg}");
    }

    #[tokio::test]
    async fn batch_failure_rolls_back() {
        let s = state();
        // First, create a node so we have a known starting point.
        QueryService::execute(
            s.databases(),
            s.metrics(),
            "default",
            "CREATE (n:Counter {val: 0})",
            None,
            None,
            None,
            false,
            None,
        )
        .await
        .unwrap();

        // Batch: mutate then fail. The mutation should be rolled back.
        let _ = QueryService::batch_execute(
            s.databases(),
            s.metrics(),
            "default",
            vec![
                BatchQuery {
                    statement: "CREATE (n:Counter {val: 99})".to_string(),
                    language: None,
                    params: None,
                },
                BatchQuery {
                    statement: "INVALID SYNTAX".to_string(),
                    language: None,
                    params: None,
                },
            ],
            None,
            false,
            None,
        )
        .await;

        // Only the original node should exist (the batch was rolled back).
        let qr = QueryService::execute(
            s.databases(),
            s.metrics(),
            "default",
            "MATCH (n:Counter) RETURN n.val AS v",
            None,
            None,
            None,
            false,
            None,
        )
        .await
        .unwrap();
        assert_eq!(qr.rows().len(), 1);
    }

    #[tokio::test]
    async fn batch_not_found_database() {
        let s = state();
        let err = QueryService::batch_execute(
            s.databases(),
            s.metrics(),
            "nope",
            vec![BatchQuery {
                statement: "MATCH (n) RETURN n".to_string(),
                language: None,
                params: None,
            }],
            None,
            false,
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    // -----------------------------------------------------------------------
    // dispatch (sync, for transport crates)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn dispatch_gql() {
        let s = state();
        let entry = s.databases().get("default").unwrap();
        let session = entry.db().session();
        let qr =
            QueryService::dispatch(&session, "MATCH (n) RETURN n LIMIT 0", None, None).unwrap();
        assert!(qr.rows().is_empty());
    }

    #[tokio::test]
    async fn dispatch_syntax_error() {
        let s = state();
        let entry = s.databases().get("default").unwrap();
        let session = entry.db().session();
        let err = QueryService::dispatch(&session, "TOTALLY BROKEN", None, None).unwrap_err();
        assert!(matches!(err, ServiceError::BadRequest(_)));
    }

    // -----------------------------------------------------------------------
    // get_session
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_session_not_found() {
        let s = state();
        let result = QueryService::get_session(s.sessions(), "no-such-id", 300);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn get_session_after_begin() {
        let s = state();
        let id = QueryService::begin_tx(s.databases(), s.sessions(), "default", false, None, None)
            .await
            .unwrap();
        let session = QueryService::get_session(s.sessions(), &id, 300);
        assert!(session.is_ok());
    }

    // -----------------------------------------------------------------------
    // dispatch: permission denied -> Forbidden mapping
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn execute_write_with_read_only_identity_returns_forbidden() {
        let s = state();
        // Use a read-only identity to attempt a write operation.
        let identity = Identity::new("readonly-user", [Role::ReadOnly]);
        let err = QueryService::execute(
            s.databases(),
            s.metrics(),
            "default",
            "CREATE (n:Test {val: 1})",
            None,
            None,
            None,
            false,
            Some(identity),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, ServiceError::Forbidden(_)),
            "expected Forbidden, got: {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // run_with_timeout
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn run_with_timeout_no_timeout() {
        let result = run_with_timeout(None, || Ok::<_, ServiceError>(42)).await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn run_with_timeout_zero_duration() {
        // Zero duration should behave like no timeout.
        let result = run_with_timeout(Some(Duration::ZERO), || Ok::<_, ServiceError>(42)).await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn run_with_timeout_generous_deadline() {
        let result = run_with_timeout(Some(Duration::from_secs(10)), || {
            Ok::<_, ServiceError>("fast")
        })
        .await;
        assert_eq!(result.unwrap(), "fast");
    }

    #[tokio::test]
    async fn run_with_timeout_propagates_error() {
        let result = run_with_timeout(None, || {
            Err::<(), _>(ServiceError::BadRequest("boom".to_string()))
        })
        .await;
        assert!(matches!(result.unwrap_err(), ServiceError::BadRequest(_)));
    }
}
