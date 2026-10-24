//! `GrafeoBackend`: implements `boltr::server::BoltBackend` for Grafeo.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use boltr::error::BoltError;
use boltr::server::{
    AuthInfo, BoltBackend, BoltRecord, ResultMetadata, ResultStream, RoutingServer, RoutingTable,
    SessionConfig, SessionHandle, SessionProperty, TransactionHandle,
};
use boltr::types::{BoltDict, BoltValue};
use dashmap::DashMap;
use parking_lot::Mutex;
use uuid::Uuid;

use grafeo_service::ServiceState;
use grafeo_service::error::ServiceError;
use grafeo_service::query::QueryService;

use crate::encode::{convert_params, grafeo_to_bolt};

#[cfg(feature = "auth")]
use crate::auth::PendingAuth;

struct GrafeoSession {
    /// The engine session queries run in. `None` until LOGON succeeds when
    /// the server requires authentication: no engine session exists for a
    /// connection that has not authenticated, so nothing can run in one even
    /// if the protocol layer let a message through (the boltr 0.2.0 RESET
    /// bypass).
    engine_session: Option<grafeo_engine::Session>,
    database: String,
    /// Identity from authentication, used when creating new sessions on database switch.
    #[cfg(feature = "auth")]
    identity: Option<grafeo_engine::auth::Identity>,
    /// Databases this token is authorized to access (empty = all).
    #[cfg(feature = "auth")]
    db_scope: Vec<String>,
}

impl GrafeoSession {
    /// The engine session, or the error for a connection that has not
    /// authenticated.
    fn engine(&self) -> Result<&grafeo_engine::Session, BoltError> {
        self.engine_session.as_ref().ok_or_else(unauthenticated)
    }

    /// The engine session, mutably, or the error for a connection that has
    /// not authenticated.
    fn engine_mut(&mut self) -> Result<&mut grafeo_engine::Session, BoltError> {
        self.engine_session.as_mut().ok_or_else(unauthenticated)
    }
}

/// The error for a message that needs an authenticated session.
fn unauthenticated() -> BoltError {
    BoltError::Authentication("authentication required".into())
}

/// Bolt backend implementation for Grafeo.
pub struct GrafeoBackend {
    state: ServiceState,
    sessions: DashMap<String, Arc<Mutex<GrafeoSession>>>,
    advertise_addr: Option<SocketAddr>,
    /// Monotonic counter for generating bookmark strings.
    bookmark_counter: AtomicU64,
    #[cfg(feature = "auth")]
    pub(crate) pending: PendingAuth,
    /// When `true` (the server has an auth provider), a session gets its
    /// engine session only once LOGON succeeds.
    #[cfg(feature = "auth")]
    auth_required: bool,
}

impl GrafeoBackend {
    pub fn new(state: ServiceState) -> Self {
        Self {
            state,
            sessions: DashMap::new(),
            advertise_addr: None,
            bookmark_counter: AtomicU64::new(1),
            #[cfg(feature = "auth")]
            pending: std::sync::Arc::new(DashMap::new()),
            #[cfg(feature = "auth")]
            auth_required: false,
        }
    }

    /// Creates a new backend with a shared `PendingAuth` map.
    ///
    /// The same map must be passed to the `BoltrAuthValidator` so that
    /// token identity flows from authentication to session creation.
    #[cfg(feature = "auth")]
    pub fn with_pending_auth(state: ServiceState, pending: PendingAuth) -> Self {
        Self {
            state,
            sessions: DashMap::new(),
            advertise_addr: None,
            bookmark_counter: AtomicU64::new(1),
            pending,
            auth_required: false,
        }
    }

    /// Requires authentication: sessions get no engine session until LOGON
    /// succeeds, and every query, transaction and database switch before it
    /// is refused. [`serve`](crate::serve) sets this when it is given an
    /// auth provider.
    #[cfg(feature = "auth")]
    #[must_use]
    pub fn with_auth_required(mut self) -> Self {
        self.auth_required = true;
        self
    }

    /// Whether sessions wait for LOGON before they get an engine session.
    fn requires_auth(&self) -> bool {
        #[cfg(feature = "auth")]
        return self.auth_required;
        #[cfg(not(feature = "auth"))]
        return false;
    }

    /// Sets the address to advertise in ROUTE responses.
    #[must_use]
    pub fn with_advertise_addr(mut self, addr: SocketAddr) -> Self {
        self.advertise_addr = Some(addr);
        self
    }

    fn get_session(&self, handle: &SessionHandle) -> Result<Arc<Mutex<GrafeoSession>>, BoltError> {
        self.sessions
            .get(&handle.0)
            .map(|entry| Arc::clone(entry.value()))
            .ok_or_else(|| BoltError::Session(format!("session '{}' not found", handle.0)))
    }
}

#[async_trait::async_trait]
impl BoltBackend for GrafeoBackend {
    async fn create_session(&self, _config: &SessionConfig) -> Result<SessionHandle, BoltError> {
        let entry = self
            .state
            .databases()
            .get("default")
            .ok_or_else(|| BoltError::Session("default database not found".into()))?;

        // With authentication on, the connection has not logged on yet: it
        // gets its engine session in `set_session_auth`.
        let engine_session = if self.requires_auth() {
            None
        } else {
            let ro = self.state.is_query_read_only();
            Some(
                tokio::task::spawn_blocking(move || {
                    let db = entry.db();
                    if ro {
                        db.session_with_role(grafeo_engine::auth::Role::ReadOnly)
                    } else {
                        db.session()
                    }
                })
                .await
                .map_err(BoltError::backend)?,
            )
        };

        let id = Uuid::new_v4().to_string();
        self.sessions.insert(
            id.clone(),
            Arc::new(Mutex::new(GrafeoSession {
                engine_session,
                database: "default".to_owned(),
                #[cfg(feature = "auth")]
                identity: None,
                #[cfg(feature = "auth")]
                db_scope: Vec::new(),
            })),
        );
        tracing::debug!(session_id = %id, "Bolt session created");
        Ok(SessionHandle(id))
    }

    async fn set_session_auth(
        &self,
        session: &SessionHandle,
        auth_info: AuthInfo,
    ) -> Result<(), BoltError> {
        // "none" scheme is now rejected by the validator, so an empty
        // principal should not occur. Treat it as an error when auth is on.
        if auth_info.principal.is_empty() {
            #[cfg(feature = "auth")]
            return Err(BoltError::Authentication("authentication required".into()));
            #[cfg(not(feature = "auth"))]
            return Ok(());
        }

        #[cfg(feature = "auth")]
        {
            let token_info = self
                .pending
                .remove(&auth_info.principal)
                .map(|(_, (info, _))| info);

            if let Some(info) = token_info {
                let identity = info.identity();
                let db_scope = info.scope.databases.clone();
                let session_arc = self.get_session(session)?;

                // Recreate the engine session with the resolved identity.
                let entry = {
                    let s = session_arc.lock();
                    self.state
                        .databases()
                        .get(&s.database)
                        .ok_or_else(|| BoltError::Session("database not found".into()))?
                };

                let ro = self.state.is_query_read_only();
                let id_clone = identity.clone();
                let engine_session = tokio::task::spawn_blocking(move || {
                    let db = entry.db();
                    let effective = grafeo_service::auth::cap_identity_read_only(id_clone, ro);
                    db.session_with_identity(effective)
                })
                .await
                .map_err(BoltError::backend)?;

                let mut s = session_arc.lock();
                s.engine_session = Some(engine_session);
                s.identity = Some(identity);
                s.db_scope = db_scope;
            } else if self.auth_required {
                // A principal the validator did not issue (or one reaped
                // before the session took it): the session stays without an
                // engine session, so nothing runs in it.
                return Err(BoltError::Authentication(
                    "authentication expired or unknown".into(),
                ));
            }
        }

        #[cfg(not(feature = "auth"))]
        let _ = (session, auth_info);

        Ok(())
    }

    async fn close_session(&self, session: &SessionHandle) -> Result<(), BoltError> {
        self.sessions.remove(&session.0);
        tracing::debug!(session_id = %session.0, "Bolt session closed");
        Ok(())
    }

    async fn configure_session(
        &self,
        session: &SessionHandle,
        property: SessionProperty,
    ) -> Result<(), BoltError> {
        match property {
            SessionProperty::Database(db_name) => {
                // Check database scope before allowing the switch.
                #[cfg(feature = "auth")]
                {
                    let session_arc = self.get_session(session)?;
                    let s = session_arc.lock();
                    if s.engine_session.is_none() {
                        return Err(unauthenticated());
                    }
                    if !s.db_scope.is_empty() && !s.db_scope.iter().any(|d| d == &db_name) {
                        return Err(BoltError::Session(format!(
                            "not authorized for database '{db_name}'"
                        )));
                    }
                }

                let entry = self
                    .state
                    .databases()
                    .get_available(&db_name)
                    .map_err(|e| BoltError::Session(e.to_string()))?;

                let ro = self.state.is_query_read_only();

                // Reuse stored identity when switching databases.
                #[cfg(feature = "auth")]
                let identity = {
                    let session_arc = self.get_session(session)?;
                    let s = session_arc.lock();
                    s.identity.clone()
                };

                let engine_session = tokio::task::spawn_blocking(move || {
                    let db = entry.db();
                    #[cfg(feature = "auth")]
                    if let Some(id) = identity {
                        let effective = grafeo_service::auth::cap_identity_read_only(id, ro);
                        return db.session_with_identity(effective);
                    }
                    if ro {
                        db.session_with_role(grafeo_engine::auth::Role::ReadOnly)
                    } else {
                        db.session()
                    }
                })
                .await
                .map_err(BoltError::backend)?;

                let session_arc = self.get_session(session)?;
                let mut s = session_arc.lock();
                s.engine_session = Some(engine_session);
                s.database = db_name;
            }
        }
        Ok(())
    }

    async fn reset_session(&self, session: &SessionHandle) -> Result<(), BoltError> {
        let entry = self
            .state
            .databases()
            .get("default")
            .ok_or_else(|| BoltError::Session("default database not found".into()))?;

        let ro = self.state.is_query_read_only();
        let session_arc = self.get_session(session)?;

        // A session that has not authenticated stays without an engine
        // session: RESET never grants what LOGON did not.
        {
            let mut s = session_arc.lock();
            if s.engine_session.is_none() {
                "default".clone_into(&mut s.database);
                return Ok(());
            }
        }

        // Preserve identity across reset.
        #[cfg(feature = "auth")]
        let identity = session_arc.lock().identity.clone();

        let engine_session = tokio::task::spawn_blocking(move || {
            let db = entry.db();
            #[cfg(feature = "auth")]
            if let Some(id) = identity {
                let effective = grafeo_service::auth::cap_identity_read_only(id, ro);
                return db.session_with_identity(effective);
            }
            if ro {
                db.session_with_role(grafeo_engine::auth::Role::ReadOnly)
            } else {
                db.session()
            }
        })
        .await
        .map_err(BoltError::backend)?;

        let mut s = session_arc.lock();
        s.engine_session = Some(engine_session);
        "default".clone_into(&mut s.database);
        Ok(())
    }

    async fn execute(
        &self,
        session: &SessionHandle,
        query: &str,
        parameters: &HashMap<String, BoltValue>,
        extra: &BoltDict,
        _transaction: Option<&TransactionHandle>,
    ) -> Result<ResultStream, BoltError> {
        let session_arc = self.get_session(session)?;
        let is_mutation = is_mutation_query(query);
        let statement = query.to_owned();
        let params = convert_params(parameters);

        // Grafeo extension: language selection via extra dict.
        let language = extra
            .get("language")
            .and_then(|v| v.as_str())
            .map(String::from);

        let result = tokio::task::spawn_blocking(move || {
            let session = session_arc.lock();
            let engine = session.engine()?;
            let params_opt = if params.is_empty() {
                None
            } else {
                Some(&params)
            };
            QueryService::dispatch(engine, &statement, language.as_deref(), params_opt).map_err(
                |e| {
                    bolt_error(e, |message| BoltError::Query {
                        code: "Neo.ClientError.Statement.SyntaxError".to_string(),
                        message,
                    })
                },
            )
        })
        .await
        .map_err(BoltError::backend)??;

        let columns = result.columns.clone();
        let records: Vec<BoltRecord> = result
            .rows()
            .iter()
            .map(|row| BoltRecord {
                values: row.iter().map(grafeo_to_bolt).collect(),
            })
            .collect();

        let mut summary = BoltDict::new();
        if let Some(ms) = result.execution_time_ms {
            summary.insert(
                "t_last".to_string(),
                BoltValue::Integer((ms * 1000.0) as i64),
            );
        }
        // Report the correct query type to drivers for routing decisions.
        let query_type = if is_mutation { "w" } else { "r" };
        summary.insert(
            "type".to_string(),
            BoltValue::String(query_type.to_string()),
        );
        if let Some(written) = grafeo_service::types::WriteCountersInfo::from_result(&result) {
            summary.insert(
                "stats".to_string(),
                BoltValue::Dict(crate::encode::write_stats(&written)),
            );
        }

        Ok(ResultStream {
            metadata: ResultMetadata {
                columns,
                extra: BoltDict::new(),
            },
            records,
            summary,
        })
    }

    async fn begin_transaction(
        &self,
        session: &SessionHandle,
        _extra: &BoltDict,
    ) -> Result<TransactionHandle, BoltError> {
        let session_arc = self.get_session(session)?;
        tokio::task::spawn_blocking(move || {
            let mut s = session_arc.lock();
            s.engine_mut()?
                .begin_transaction()
                .map_err(|e| bolt_error(ServiceError::from_engine(e), BoltError::Transaction))
        })
        .await
        .map_err(BoltError::backend)??;

        Ok(TransactionHandle(Uuid::new_v4().to_string()))
    }

    async fn commit(
        &self,
        session: &SessionHandle,
        _transaction: &TransactionHandle,
    ) -> Result<BoltDict, BoltError> {
        let session_arc = self.get_session(session)?;
        tokio::task::spawn_blocking(move || {
            let mut s = session_arc.lock();
            s.engine_mut()?
                .commit()
                .map_err(|e| bolt_error(ServiceError::from_engine(e), BoltError::Transaction))
        })
        .await
        .map_err(BoltError::backend)??;

        // Return a monotonic bookmark for causal consistency.
        let seq = self.bookmark_counter.fetch_add(1, Ordering::Relaxed);
        let mut meta = BoltDict::new();
        meta.insert(
            "bookmark".to_string(),
            BoltValue::String(format!("grafeo:tx:{seq}")),
        );
        Ok(meta)
    }

    async fn rollback(
        &self,
        session: &SessionHandle,
        _transaction: &TransactionHandle,
    ) -> Result<(), BoltError> {
        let session_arc = self.get_session(session)?;
        tokio::task::spawn_blocking(move || {
            let mut s = session_arc.lock();
            s.engine_mut()?
                .rollback()
                .map_err(|e| bolt_error(ServiceError::from_engine(e), BoltError::Transaction))
        })
        .await
        .map_err(BoltError::backend)?
    }

    async fn get_server_info(&self) -> Result<BoltDict, BoltError> {
        Ok(BoltDict::from([(
            "server".to_string(),
            BoltValue::String(format!("GrafeoDB/{}", env!("CARGO_PKG_VERSION"))),
        )]))
    }

    async fn route(
        &self,
        _routing_context: &BoltDict,
        _bookmarks: &[String],
        db: Option<&str>,
    ) -> Result<RoutingTable, BoltError> {
        let addr = self
            .advertise_addr
            .map_or_else(|| "localhost:7687".to_string(), |a| a.to_string());

        let db_name = db.unwrap_or("default").to_string();

        // Single-server routing: this node serves all roles.
        // In read-only mode, omit the WRITE role.
        let mut servers = Vec::new();
        if !self.state.databases().is_read_only() {
            servers.push(RoutingServer {
                addresses: vec![addr.clone()],
                role: "WRITE".to_string(),
            });
        }
        servers.push(RoutingServer {
            addresses: vec![addr.clone()],
            role: "READ".to_string(),
        });
        servers.push(RoutingServer {
            addresses: vec![addr],
            role: "ROUTE".to_string(),
        });

        Ok(RoutingTable {
            ttl: 300,
            db: db_name,
            servers,
        })
    }
}

/// Maps a service error to the Bolt failure drivers act on: a conflict with a
/// concurrent transaction is a transient error (Neo4j drivers retry managed
/// transactions only on those), an unavailable database the transient
/// `DatabaseUnavailable`, a permission error `Forbidden`; anything else gets
/// `other`, with the message.
fn bolt_error(error: ServiceError, other: impl FnOnce(String) -> BoltError) -> BoltError {
    match error {
        ServiceError::Forbidden(msg) => BoltError::Forbidden(msg),
        ServiceError::TransactionConflict(message) => BoltError::Query {
            code: "Neo.TransientError.Transaction.Outdated".to_string(),
            message,
        },
        ServiceError::Unavailable(message) => BoltError::Query {
            code: "Neo.TransientError.General.DatabaseUnavailable".to_string(),
            message,
        },
        error => other(error.to_string()),
    }
}

/// Heuristic to detect mutation queries from the statement text.
///
/// Used to set the `type` field in RUN response metadata ("r" for read,
/// "w" for write). Neo4j drivers use this for routing decisions.
fn is_mutation_query(statement: &str) -> bool {
    let upper = statement.to_ascii_uppercase();
    // Check for mutation keywords that indicate write operations
    [
        "INSERT ", "CREATE ", "DELETE ", "SET ", "REMOVE ", "MERGE ", "DROP ", "ALTER ",
    ]
    .iter()
    .any(|kw| upper.contains(kw))
}

#[cfg(all(test, feature = "auth"))]
mod tests {
    use super::*;

    fn config() -> SessionConfig {
        SessionConfig {
            user_agent: "test".to_string(),
            database: None,
        }
    }

    fn backend() -> (GrafeoBackend, ServiceState) {
        let state = ServiceState::new_in_memory_with_auth(300, "secret".to_string());
        (
            GrafeoBackend::new(state.clone()).with_auth_required(),
            state,
        )
    }

    fn is_unauthenticated(error: &BoltError) -> bool {
        matches!(error, BoltError::Authentication(_))
    }

    /// The second guard of the RESET bypass: whatever the protocol layer lets
    /// through, a session that never logged on has no engine session, so a
    /// RUN, a BEGIN or a database switch is refused, also after RESET.
    #[tokio::test]
    async fn a_session_without_logon_runs_nothing_even_after_reset() {
        let (backend, state) = backend();
        let session = backend.create_session(&config()).await.unwrap();

        let run = backend
            .execute(
                &session,
                "INSERT (:Bypass)",
                &HashMap::new(),
                &BoltDict::new(),
                None,
            )
            .await;
        assert!(is_unauthenticated(&run.unwrap_err()));

        backend.reset_session(&session).await.unwrap();
        let run = backend
            .execute(
                &session,
                "INSERT (:Bypass)",
                &HashMap::new(),
                &BoltDict::new(),
                None,
            )
            .await;
        assert!(is_unauthenticated(&run.unwrap_err()));
        let begin = backend.begin_transaction(&session, &BoltDict::new()).await;
        assert!(is_unauthenticated(&begin.unwrap_err()));
        let switch = backend
            .configure_session(&session, SessionProperty::Database("default".into()))
            .await;
        assert!(is_unauthenticated(&switch.unwrap_err()));

        let db = state.databases().get("default").unwrap().db();
        assert_eq!(db.node_count().unwrap(), 0, "nothing was written");
    }

    /// A principal the validator never issued gives no engine session.
    #[tokio::test]
    async fn an_unknown_principal_is_refused() {
        let (backend, _state) = backend();
        let session = backend.create_session(&config()).await.unwrap();
        let auth = backend
            .set_session_auth(
                &session,
                AuthInfo {
                    principal: "forged".to_string(),
                    credentials_expired: false,
                },
            )
            .await;
        assert!(is_unauthenticated(&auth.unwrap_err()));
        let run = backend
            .execute(
                &session,
                "RETURN 1",
                &HashMap::new(),
                &BoltDict::new(),
                None,
            )
            .await;
        assert!(is_unauthenticated(&run.unwrap_err()));
    }

    /// Without an auth provider a session runs at once, as before.
    #[tokio::test]
    async fn without_auth_a_session_runs_at_once() {
        let state = ServiceState::new_in_memory(300);
        let backend = GrafeoBackend::new(state);
        let session = backend.create_session(&config()).await.unwrap();
        let result = backend
            .execute(
                &session,
                "RETURN 1 AS x",
                &HashMap::new(),
                &BoltDict::new(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(result.records.len(), 1);
    }
}
