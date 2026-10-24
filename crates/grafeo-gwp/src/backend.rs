//! GqlBackend trait implementation for Grafeo.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use dashmap::DashMap;
use gwp::error::GqlError;
use gwp::proto;
use gwp::server::{
    AdminStats, AdminValidationResult, AdminWalStatus, CatalogContext, CreateGraphConfig,
    GqlBackend, GraphInfo, GraphTypeInfo, HybridSearchParams, IndexDefinition, ResetTarget,
    ResultFrame, ResultStream, SchemaInfo, SearchHit, SessionConfig, SessionHandle,
    SessionProperty, TextSearchParams, TransactionHandle, ValidationDiagnostic, VectorSearchParams,
};
use gwp::status;
use gwp::types::Value as GwpValue;
use parking_lot::Mutex;
use uuid::Uuid;

use grafeo_service::ServiceState;
use grafeo_service::admin::AdminService;
use grafeo_service::error::ServiceError;
use grafeo_service::query::QueryService;
use grafeo_service::search::SearchService;

use crate::encode::{convert_params, grafeo_to_gwp};

/// Default schema name. Grafeo uses a flat database namespace, so all
/// graphs live in a single implicit schema.
const DEFAULT_SCHEMA: &str = "default";

/// The database a new GWP session works in.
const DEFAULT_DATABASE: &str = "default";

/// The session parameter that picks a session's database. ISO/IEC 39075
/// names implementation-defined session parameters with a leading `_`.
const DATABASE_PARAMETER: &str = "_database";

/// A GWP session backed by a grafeo-engine `Session`.
struct GrafeoSession {
    engine_session: grafeo_engine::Session,
    database: String,
    /// Query language override (e.g. "cypher"). When `None`, defaults to GQL.
    language: Option<String>,
    /// Identity from authentication, used when creating new sessions on database switch.
    #[cfg(feature = "auth")]
    identity: Option<grafeo_engine::auth::Identity>,
    /// Databases this token is authorized to access (empty = all).
    #[cfg(feature = "auth")]
    db_scope: Vec<String>,
}

#[cfg(feature = "auth")]
use crate::auth::PendingAuth;

/// GQL Wire Protocol backend for Grafeo.
///
/// Implements `GqlBackend` by delegating to grafeo-engine sessions.
/// Each GWP session maps to one engine session on a specific database.
/// All engine operations run via `spawn_blocking` to avoid blocking
/// the async runtime.
pub struct GrafeoBackend {
    state: ServiceState,
    sessions: DashMap<String, Arc<Mutex<GrafeoSession>>>,
    #[cfg(feature = "auth")]
    pub(crate) pending: PendingAuth,
    /// Set when the server authenticates sessions: catalog calls then need
    /// a session, since one without has no identity to check.
    #[cfg(feature = "auth")]
    catalog_needs_session: bool,
}

impl GrafeoBackend {
    /// Creates a new backend wrapping the shared service state.
    pub fn new(state: ServiceState) -> Self {
        Self {
            state,
            sessions: DashMap::new(),
            #[cfg(feature = "auth")]
            pending: std::sync::Arc::new(DashMap::new()),
            #[cfg(feature = "auth")]
            catalog_needs_session: false,
        }
    }

    /// Creates a new backend with a shared `PendingAuth` map.
    ///
    /// The same map must be passed to the `GwpAuthValidator` so that
    /// token identity flows from authentication to session creation.
    #[cfg(feature = "auth")]
    pub fn with_pending_auth(state: ServiceState, pending: PendingAuth) -> Self {
        Self {
            state,
            sessions: DashMap::new(),
            pending,
            catalog_needs_session: false,
        }
    }

    /// Refuses catalog calls that name no session (the server authenticates
    /// sessions). [`serve`](crate::serve) sets this with an auth provider.
    #[cfg(feature = "auth")]
    #[must_use]
    pub fn with_catalog_sessions_required(mut self) -> Self {
        self.catalog_needs_session = true;
        self
    }

    /// Returns `true` if client-facing queries should be read-only.
    fn query_read_only(&self) -> bool {
        self.state.is_query_read_only()
    }

    /// Looks up an internal session by handle.
    #[allow(clippy::result_large_err)]
    fn get_session(&self, handle: &SessionHandle) -> Result<Arc<Mutex<GrafeoSession>>, GqlError> {
        self.sessions
            .get(&handle.0)
            .map(|entry| Arc::clone(entry.value()))
            .ok_or_else(|| GqlError::Session(format!("session '{}' not found", handle.0)))
    }

    /// Opens an engine session on database `name` for a GWP session acting as
    /// `identity` (capped to read-only when the server's queries are).
    async fn open_engine_session(
        &self,
        name: &str,
        #[cfg(feature = "auth")] identity: Option<grafeo_engine::auth::Identity>,
    ) -> Result<grafeo_engine::Session, GqlError> {
        let entry = self
            .state
            .databases()
            .get_available(name)
            .map_err(|e| GqlError::Session(e.to_string()))?;
        let ro = self.query_read_only();
        tokio::task::spawn_blocking(move || {
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
        .map_err(GqlError::backend)
    }

    /// Moves a GWP session to database `name` (the `_database` parameter):
    /// a new engine session there, refused while a transaction is open,
    /// since a transaction cannot span two databases (two GQL-catalogs).
    async fn switch_database(
        &self,
        session_arc: &Arc<Mutex<GrafeoSession>>,
        name: String,
    ) -> Result<(), GqlError> {
        #[cfg(feature = "auth")]
        let identity = {
            let s = session_arc.lock();
            if !s.db_scope.is_empty() && !s.db_scope.iter().any(|d| d == &name) {
                return Err(GqlError::Session(format!(
                    "not authorized for database '{name}'"
                )));
            }
            s.identity.clone()
        };
        let open_transaction = || {
            GqlError::status(
                status::ACTIVE_TRANSACTION,
                format!("cannot switch to database '{name}' while a transaction is open"),
            )
        };
        if session_arc.lock().engine_session.in_transaction() {
            return Err(open_transaction());
        }
        let engine_session = self
            .open_engine_session(
                &name,
                #[cfg(feature = "auth")]
                identity,
            )
            .await?;
        let mut s = session_arc.lock();
        // Checked again: a transaction may have begun while the session opened.
        if s.engine_session.in_transaction() {
            return Err(open_transaction());
        }
        s.engine_session = engine_session;
        s.database = name;
        Ok(())
    }

    /// The database a catalog call works in, and the identity it acts as:
    /// the calling session's, or the default database for a call without
    /// one (refused when the server authenticates sessions).
    #[allow(clippy::result_large_err)]
    fn catalog_target(&self, ctx: &CatalogContext) -> Result<CatalogTarget, GqlError> {
        match &ctx.session {
            Some(handle) => {
                let session_arc = self.get_session(handle)?;
                let s = session_arc.lock();
                Ok(CatalogTarget {
                    database: s.database.clone(),
                    #[cfg(feature = "auth")]
                    identity: s.identity.clone(),
                })
            }
            None => {
                #[cfg(feature = "auth")]
                if self.catalog_needs_session {
                    return Err(GqlError::Session(
                        "catalog calls need an authenticated session (pass its session id)"
                            .to_owned(),
                    ));
                }
                Ok(CatalogTarget {
                    database: DEFAULT_DATABASE.to_owned(),
                    #[cfg(feature = "auth")]
                    identity: None,
                })
            }
        }
    }

    /// Runs `statement` in `schema` of the catalog target, on an engine
    /// session of its own (the caller's session keeps its graph and schema).
    async fn catalog_execute(
        &self,
        target: &CatalogTarget,
        schema: &str,
        statement: String,
    ) -> Result<grafeo_engine::database::QueryResult, GqlError> {
        let engine_session = self
            .open_engine_session(
                &target.database,
                #[cfg(feature = "auth")]
                target.identity.clone(),
            )
            .await?;
        let schema = schema_name(schema).map(str::to_owned);
        tokio::task::spawn_blocking(move || {
            if let Some(schema) = schema {
                engine_session.set_schema(&schema);
            }
            engine_session.execute(&statement)
        })
        .await
        .map_err(GqlError::backend)?
        .map_err(|e| GqlError::Session(e.to_string()))
    }

    /// Whether graph `name` exists in `schema` of the catalog target.
    #[allow(clippy::result_large_err)]
    fn graph_exists(
        &self,
        target: &CatalogTarget,
        schema: &str,
        name: &str,
    ) -> Result<bool, GqlError> {
        let entry = self
            .state
            .databases()
            .get_available(&target.database)
            .map_err(|e| GqlError::Session(e.to_string()))?;
        Ok(entry.db().graph_in(schema_name(schema), name).is_ok())
    }

    /// Describes graph `name` in `schema` of the catalog target. The storage
    /// settings are its database's: a graph lives in its database.
    #[allow(clippy::result_large_err)]
    fn graph_info(
        &self,
        target: &CatalogTarget,
        schema: &str,
        name: &str,
    ) -> Result<GraphInfo, GqlError> {
        let entry = self
            .state
            .databases()
            .get_available(&target.database)
            .map_err(|e| GqlError::Session(e.to_string()))?;
        let db = entry.db();
        let handle = db
            .graph_in(schema_name(schema), name)
            .map_err(|_| GqlError::Session(format!("graph '{name}' not found")))?;
        let engine_error = |e| GqlError::backend(ServiceError::from_engine(e));
        let snapshot = handle.snapshot().map_err(engine_error)?;
        let view = snapshot.view();
        Ok(GraphInfo {
            schema: if schema.is_empty() {
                DEFAULT_SCHEMA
            } else {
                schema
            }
            .to_owned(),
            name: name.to_owned(),
            node_count: snapshot.store().node_count(view).map_err(engine_error)? as u64,
            edge_count: snapshot.store().edge_count(view).map_err(engine_error)? as u64,
            graph_type: entry.metadata.database_type.clone(),
            storage_mode: entry.metadata.storage_mode.clone(),
            memory_limit_bytes: db.memory_limit().map(|v| v as u64),
            backward_edges: Some(entry.metadata.backward_edges),
            threads: Some(entry.metadata.threads as u32),
        })
    }
}

#[tonic::async_trait]
impl GqlBackend for GrafeoBackend {
    async fn create_session(&self, config: &SessionConfig) -> Result<SessionHandle, GqlError> {
        // Resolve identity from auth_info principal (nonce -> TokenInfo -> Identity).
        // When auth_info is None, no auth provider was configured: allow unauthenticated.
        // When auth_info is Some but the nonce lookup fails, reject (stale or replayed).
        #[cfg(feature = "auth")]
        let (identity, db_scope): (Option<grafeo_engine::auth::Identity>, Vec<String>) =
            match config.auth_info.as_ref() {
                Some(info) => {
                    let (_, (token_info, _)) =
                        self.pending.remove(&info.principal).ok_or_else(|| {
                            GqlError::Protocol("auth session expired or invalid".to_owned())
                        })?;
                    (
                        Some(token_info.identity()),
                        token_info.scope.databases.clone(),
                    )
                }
                None => (None, vec![]),
            };

        #[cfg(not(feature = "auth"))]
        let _ = config;

        let engine_session = self
            .open_engine_session(
                DEFAULT_DATABASE,
                #[cfg(feature = "auth")]
                identity.clone(),
            )
            .await?;

        let id = Uuid::new_v4().to_string();
        self.sessions.insert(
            id.clone(),
            Arc::new(Mutex::new(GrafeoSession {
                engine_session,
                database: DEFAULT_DATABASE.to_owned(),
                language: None,
                #[cfg(feature = "auth")]
                identity,
                #[cfg(feature = "auth")]
                db_scope,
            })),
        );

        tracing::debug!(session_id = %id, "GWP session created");
        Ok(SessionHandle(id))
    }

    async fn close_session(&self, session: &SessionHandle) -> Result<(), GqlError> {
        self.sessions.remove(&session.0);
        tracing::debug!(session_id = %session.0, "GWP session closed");
        Ok(())
    }

    async fn configure_session(
        &self,
        session: &SessionHandle,
        property: SessionProperty,
    ) -> Result<(), GqlError> {
        let session_arc = self.get_session(session)?;
        match property {
            // ISO/IEC 39075 7.1: the session graph, a graph of the session's
            // database (and schema). An open transaction stays open.
            SessionProperty::Graph(name) => {
                let database = session_arc.lock().database.clone();
                let entry = self
                    .state
                    .databases()
                    .get_available(&database)
                    .map_err(|e| GqlError::Session(e.to_string()))?;
                tokio::task::spawn_blocking(move || {
                    let s = session_arc.lock();
                    let schema = s.engine_session.current_schema();
                    match entry.db().graph_in(schema.as_deref(), &name) {
                        Ok(_) => {
                            s.engine_session.use_graph(&name);
                            Ok(())
                        }
                        Err(e) => Err(GqlError::status(status::INVALID_REFERENCE, e.to_string())),
                    }
                })
                .await
                .map_err(GqlError::backend)??;
            }
            SessionProperty::Schema(schema_name) => {
                let s = session_arc.lock();
                s.engine_session.set_schema(&schema_name);
                tracing::debug!(schema = %schema_name, "GWP session schema set");
            }
            SessionProperty::Parameter { name, value } if name == DATABASE_PARAMETER => {
                let GwpValue::String(db_name) = value else {
                    return Err(GqlError::Session(format!(
                        "{DATABASE_PARAMETER} must be a database name"
                    )));
                };
                self.switch_database(&session_arc, db_name).await?;
            }
            SessionProperty::Parameter { name, value } if name == "language" => {
                if let GwpValue::String(ref lang) = value {
                    tracing::info!(language = %lang, "GWP session language set");
                    session_arc.lock().language = Some(lang.clone());
                } else {
                    tracing::warn!(?value, "language parameter is not a string");
                }
            }
            SessionProperty::TimeZone(_) | SessionProperty::Parameter { .. } => {}
        }
        Ok(())
    }

    async fn reset_session(
        &self,
        session: &SessionHandle,
        target: ResetTarget,
    ) -> Result<(), GqlError> {
        // ISO/IEC 39075 7.2: a reset changes session state only; an open
        // transaction stays open, and the session keeps its database.
        let session_arc = self.get_session(session)?;
        let mut s = session_arc.lock();
        match target {
            ResetTarget::Schema => s.engine_session.reset_schema(),
            ResetTarget::Graph => s.engine_session.reset_graph(),
            ResetTarget::TimeZone => s.engine_session.reset_time_zone(),
            ResetTarget::Parameters => s.language = None,
            ResetTarget::All => {
                s.engine_session.reset_session();
                s.language = None;
            }
        }
        Ok(())
    }

    async fn execute(
        &self,
        session: &SessionHandle,
        statement: &str,
        parameters: &HashMap<String, GwpValue>,
        _transaction: Option<&TransactionHandle>,
    ) -> Result<Pin<Box<dyn ResultStream>>, GqlError> {
        let session_arc = self.get_session(session)?;
        let statement = statement.to_owned();
        let params = convert_params(parameters);

        let result = tokio::task::spawn_blocking(move || {
            let session = session_arc.lock();
            if let Some(ref lang) = session.language {
                // Language override set via Configure, route through dispatch
                let params_opt = if params.is_empty() {
                    None
                } else {
                    Some(params)
                };
                QueryService::dispatch(
                    &session.engine_session,
                    &statement,
                    Some(lang.as_str()),
                    params_opt.as_ref(),
                )
            } else if params.is_empty() {
                session
                    .engine_session
                    .execute(&statement)
                    .map_err(ServiceError::from_engine)
            } else {
                session
                    .engine_session
                    .execute_with_params(&statement, params)
                    .map_err(ServiceError::from_engine)
            }
        })
        .await
        .map_err(GqlError::backend)?
        .map_err(|e| {
            gql_error(e, |message| {
                GqlError::status(status::INVALID_SYNTAX, message)
            })
        })?;

        Ok(Box::pin(GrafeoResultStream::from_query_result(result)))
    }

    async fn begin_transaction(
        &self,
        session: &SessionHandle,
        _mode: proto::TransactionMode,
    ) -> Result<TransactionHandle, GqlError> {
        let session_arc = self.get_session(session)?;

        tokio::task::spawn_blocking(move || {
            let mut s = session_arc.lock();
            s.engine_session.begin_transaction()
        })
        .await
        .map_err(GqlError::backend)?
        .map_err(|e| GqlError::Transaction(e.to_string()))?;

        let tx_id = Uuid::new_v4().to_string();
        Ok(TransactionHandle(tx_id))
    }

    async fn commit(
        &self,
        session: &SessionHandle,
        _transaction: &TransactionHandle,
    ) -> Result<(), GqlError> {
        let session_arc = self.get_session(session)?;

        tokio::task::spawn_blocking(move || {
            let mut s = session_arc.lock();
            s.engine_session.commit()
        })
        .await
        .map_err(GqlError::backend)?
        .map_err(|e| gql_error(ServiceError::from_engine(e), GqlError::Transaction))
    }

    async fn rollback(
        &self,
        session: &SessionHandle,
        _transaction: &TransactionHandle,
    ) -> Result<(), GqlError> {
        let session_arc = self.get_session(session)?;

        tokio::task::spawn_blocking(move || {
            let mut s = session_arc.lock();
            s.engine_session.rollback()
        })
        .await
        .map_err(GqlError::backend)?
        .map_err(|e| GqlError::Transaction(e.to_string()))
    }

    // -----------------------------------------------------------------
    // Catalog: the GQL-catalog of the calling session's database (ISO/IEC
    // 39075 4.2.5): its engine schemas and graphs, the namespace GQL
    // `CREATE GRAPH` uses. A call without a session works in `default`.
    // -----------------------------------------------------------------

    async fn list_schemas(&self) -> Result<Vec<SchemaInfo>, GqlError> {
        self.list_schemas_in(&CatalogContext::default()).await
    }

    async fn list_schemas_in(&self, ctx: &CatalogContext) -> Result<Vec<SchemaInfo>, GqlError> {
        let target = self.catalog_target(ctx)?;
        let named = AdminService::list_schemas(
            self.state.databases(),
            self.state.metrics(),
            &target.database,
        )
        .await
        .map_err(|e| GqlError::Session(e.to_string()))?;
        let mut schemas = Vec::with_capacity(named.len() + 1);
        for name in std::iter::once(DEFAULT_SCHEMA.to_owned()).chain(named) {
            let graphs = self
                .catalog_execute(&target, &name, "SHOW GRAPHS".to_owned())
                .await?;
            schemas.push(SchemaInfo {
                name,
                graph_count: graphs.rows().len() as u32,
                graph_type_count: 0,
            });
        }
        Ok(schemas)
    }

    async fn create_schema(&self, name: &str, if_not_exists: bool) -> Result<(), GqlError> {
        self.create_schema_in(&CatalogContext::default(), name, if_not_exists)
            .await
    }

    async fn create_schema_in(
        &self,
        ctx: &CatalogContext,
        name: &str,
        if_not_exists: bool,
    ) -> Result<(), GqlError> {
        let target = self.catalog_target(ctx)?;
        let created = AdminService::create_schema(
            self.state.databases(),
            self.state.metrics(),
            &target.database,
            name,
        )
        .await;
        match created {
            Ok(true) => Ok(()),
            Ok(false) if if_not_exists => Ok(()),
            Ok(false) => Err(GqlError::Session(format!("schema '{name}' already exists"))),
            Err(e) => Err(GqlError::Session(e.to_string())),
        }
    }

    async fn drop_schema(&self, name: &str, if_exists: bool) -> Result<bool, GqlError> {
        self.drop_schema_in(&CatalogContext::default(), name, if_exists)
            .await
    }

    async fn drop_schema_in(
        &self,
        ctx: &CatalogContext,
        name: &str,
        if_exists: bool,
    ) -> Result<bool, GqlError> {
        let target = self.catalog_target(ctx)?;
        let dropped = AdminService::drop_schema(
            self.state.databases(),
            self.state.metrics(),
            &target.database,
            name,
        )
        .await
        .map_err(|e| GqlError::Session(e.to_string()))?;
        match (dropped, if_exists) {
            (true, _) => Ok(true),
            (false, true) => Ok(false),
            (false, false) => Err(GqlError::Session(format!("schema '{name}' not found"))),
        }
    }

    async fn list_graphs(&self, schema: &str) -> Result<Vec<GraphInfo>, GqlError> {
        self.list_graphs_in(&CatalogContext::default(), schema)
            .await
    }

    async fn list_graphs_in(
        &self,
        ctx: &CatalogContext,
        schema: &str,
    ) -> Result<Vec<GraphInfo>, GqlError> {
        let target = self.catalog_target(ctx)?;
        let names = self
            .catalog_execute(&target, schema, "SHOW GRAPHS".to_owned())
            .await?;
        names
            .rows()
            .iter()
            .filter_map(|row| row.first().and_then(|v| v.as_str()).map(str::to_owned))
            .map(|name| self.graph_info(&target, schema, &name))
            .collect()
    }

    async fn create_graph(&self, config: CreateGraphConfig) -> Result<GraphInfo, GqlError> {
        self.create_graph_in(&CatalogContext::default(), config)
            .await
    }

    async fn create_graph_in(
        &self,
        ctx: &CatalogContext,
        config: CreateGraphConfig,
    ) -> Result<GraphInfo, GqlError> {
        use gwp::server::GraphTypeSpec;

        // ISO/IEC 39075 12.4: a statement takes OR REPLACE or IF NOT EXISTS,
        // not both.
        if config.or_replace && config.if_not_exists {
            return Err(GqlError::Session(
                "OR REPLACE and IF NOT EXISTS cannot both be set".to_owned(),
            ));
        }
        refuse_database_options(&config)?;
        let target = self.catalog_target(ctx)?;
        if let Some(GraphTypeSpec::Named(type_name)) = &config.type_spec {
            let database_model = self
                .state
                .databases()
                .get(&target.database)
                .map(|e| e.metadata.database_type.clone())
                .unwrap_or_default();
            let model = if database_model.contains("rdf") || database_model.contains("owl") {
                "rdf"
            } else {
                "lpg"
            };
            if !type_name.eq_ignore_ascii_case(model) {
                return Err(GqlError::Session(format!(
                    "graph type '{type_name}' is not supported: a graph has its database's model \
                     ({model})"
                )));
            }
        }
        let exists = self.graph_exists(&target, &config.schema, &config.name)?;
        if exists && !config.or_replace && !config.if_not_exists {
            return Err(GqlError::Session(format!(
                "graph '{}' already exists",
                config.name
            )));
        }
        if config.or_replace && config.copy_of.as_deref() == Some(config.name.as_str()) {
            return Err(GqlError::Session(format!(
                "graph '{}' cannot replace itself with a copy of itself",
                config.name
            )));
        }
        if !(exists && config.if_not_exists) {
            // Engine 0.6 parses `CREATE OR REPLACE GRAPH` only for graph types,
            // so OR REPLACE drops the old graph first (two steps, not atomic).
            if config.or_replace && exists {
                self.catalog_execute(
                    &target,
                    &config.schema,
                    format!("DROP GRAPH IF EXISTS {}", quote(&config.name)),
                )
                .await?;
            }
            let mut statement = String::from("CREATE GRAPH ");
            if config.if_not_exists {
                statement.push_str("IF NOT EXISTS ");
            }
            statement.push_str(&quote(&config.name));
            if let Some(source) = &config.copy_of {
                statement.push_str(" AS COPY OF ");
                statement.push_str(&quote(source));
            }
            self.catalog_execute(&target, &config.schema, statement)
                .await?;
        }
        self.graph_info(&target, &config.schema, &config.name)
    }

    async fn drop_graph(
        &self,
        schema: &str,
        name: &str,
        if_exists: bool,
    ) -> Result<bool, GqlError> {
        self.drop_graph_in(&CatalogContext::default(), schema, name, if_exists)
            .await
    }

    async fn drop_graph_in(
        &self,
        ctx: &CatalogContext,
        schema: &str,
        name: &str,
        if_exists: bool,
    ) -> Result<bool, GqlError> {
        let target = self.catalog_target(ctx)?;
        if !self.graph_exists(&target, schema, name)? {
            return if if_exists {
                Ok(false)
            } else {
                Err(GqlError::Session(format!("graph '{name}' not found")))
            };
        }
        self.catalog_execute(
            &target,
            schema,
            format!("DROP GRAPH IF EXISTS {}", quote(name)),
        )
        .await?;
        Ok(true)
    }

    async fn get_graph_info(&self, schema: &str, name: &str) -> Result<GraphInfo, GqlError> {
        self.get_graph_info_in(&CatalogContext::default(), schema, name)
            .await
    }

    async fn get_graph_info_in(
        &self,
        ctx: &CatalogContext,
        schema: &str,
        name: &str,
    ) -> Result<GraphInfo, GqlError> {
        let target = self.catalog_target(ctx)?;
        self.graph_info(&target, schema, name)
    }

    // -----------------------------------------------------------------
    // Catalog: Graph Type operations (stubs)
    // -----------------------------------------------------------------

    async fn list_graph_types(&self, _schema: &str) -> Result<Vec<GraphTypeInfo>, GqlError> {
        Ok(Vec::new())
    }

    async fn create_graph_type(
        &self,
        _schema: &str,
        _name: &str,
        _if_not_exists: bool,
        _or_replace: bool,
    ) -> Result<(), GqlError> {
        Err(GqlError::Session("graph types not supported".to_owned()))
    }

    async fn drop_graph_type(
        &self,
        _schema: &str,
        _name: &str,
        _if_exists: bool,
    ) -> Result<bool, GqlError> {
        Err(GqlError::Session("graph types not supported".to_owned()))
    }

    // -----------------------------------------------------------------
    // Admin operations
    // -----------------------------------------------------------------

    async fn get_graph_stats(&self, graph: &str) -> Result<AdminStats, GqlError> {
        let stats = AdminService::database_stats(self.state.databases(), graph)
            .await
            .map_err(|e| GqlError::Session(e.to_string()))?;

        Ok(AdminStats {
            node_count: stats.node_count as u64,
            edge_count: stats.edge_count as u64,
            label_count: stats.label_count as u64,
            edge_type_count: stats.edge_type_count as u64,
            property_key_count: stats.property_key_count as u64,
            index_count: stats.index_count as u64,
            memory_bytes: stats.memory_bytes as u64,
            disk_bytes: stats.disk_bytes.map(|v| v as u64),
        })
    }

    async fn wal_status(&self, graph: &str) -> Result<AdminWalStatus, GqlError> {
        let status = AdminService::wal_status(self.state.databases(), graph)
            .await
            .map_err(|e| GqlError::Session(e.to_string()))?;

        Ok(AdminWalStatus {
            enabled: status.enabled,
            path: status.path,
            size_bytes: status.size_bytes as u64,
            record_count: status.record_count as u64,
            last_checkpoint: status.last_checkpoint,
            current_epoch: status.current_epoch,
        })
    }

    async fn wal_checkpoint(&self, graph: &str) -> Result<(), GqlError> {
        AdminService::wal_checkpoint(self.state.databases(), graph)
            .await
            .map_err(|e| GqlError::Session(e.to_string()))
    }

    async fn validate(&self, graph: &str) -> Result<AdminValidationResult, GqlError> {
        let result = AdminService::validate(self.state.databases(), graph)
            .await
            .map_err(|e| GqlError::Session(e.to_string()))?;

        Ok(AdminValidationResult {
            valid: result.valid,
            errors: result
                .errors
                .into_iter()
                .map(|e| ValidationDiagnostic {
                    code: e.code,
                    message: e.message,
                    context: e.context,
                })
                .collect(),
            warnings: result
                .warnings
                .into_iter()
                .map(|w| ValidationDiagnostic {
                    code: w.code,
                    message: w.message,
                    context: w.context,
                })
                .collect(),
        })
    }

    async fn create_index(&self, graph: &str, index: IndexDefinition) -> Result<(), GqlError> {
        let service_index = match index {
            IndexDefinition::Property { property } => {
                grafeo_service::types::IndexDef::Property { property }
            }
            IndexDefinition::Vector {
                label,
                property,
                dimensions,
                metric,
                m,
                ef_construction,
            } => grafeo_service::types::IndexDef::Vector {
                label,
                property,
                dimensions,
                metric,
                m,
                ef_construction,
            },
            IndexDefinition::Text { label, property } => {
                grafeo_service::types::IndexDef::Text { label, property }
            }
        };

        AdminService::create_index(self.state.databases(), graph, service_index)
            .await
            .map_err(|e| GqlError::Session(e.to_string()))
    }

    async fn drop_index(&self, graph: &str, index: IndexDefinition) -> Result<bool, GqlError> {
        let service_index = match index {
            IndexDefinition::Property { property } => {
                grafeo_service::types::IndexDef::Property { property }
            }
            IndexDefinition::Vector {
                label,
                property,
                dimensions,
                metric,
                m,
                ef_construction,
            } => grafeo_service::types::IndexDef::Vector {
                label,
                property,
                dimensions,
                metric,
                m,
                ef_construction,
            },
            IndexDefinition::Text { label, property } => {
                grafeo_service::types::IndexDef::Text { label, property }
            }
        };

        AdminService::drop_index(self.state.databases(), graph, service_index)
            .await
            .map_err(|e| GqlError::Session(e.to_string()))
    }

    // -----------------------------------------------------------------
    // Search operations
    // -----------------------------------------------------------------

    async fn vector_search(&self, req: VectorSearchParams) -> Result<Vec<SearchHit>, GqlError> {
        let service_req = grafeo_service::types::VectorSearchReq {
            database: req.graph.clone(),
            label: req.label,
            property: req.property,
            query_vector: req.query_vector,
            k: req.k,
            ef: req.ef,
            filters: req
                .filters
                .into_iter()
                .map(|(k, v)| (k, crate::encode::gwp_to_grafeo_common(&v)))
                .collect(),
        };

        let hits = SearchService::vector_search(self.state.databases(), &req.graph, service_req)
            .await
            .map_err(|e| GqlError::Session(e.to_string()))?;

        Ok(hits
            .into_iter()
            .map(|h| SearchHit {
                node_id: h.node_id,
                score: h.score,
                properties: HashMap::new(),
            })
            .collect())
    }

    async fn text_search(&self, req: TextSearchParams) -> Result<Vec<SearchHit>, GqlError> {
        let service_req = grafeo_service::types::TextSearchReq {
            database: req.graph.clone(),
            label: req.label,
            property: req.property,
            query: req.query,
            k: req.k,
        };

        let hits = SearchService::text_search(self.state.databases(), &req.graph, service_req)
            .await
            .map_err(|e| GqlError::Session(e.to_string()))?;

        Ok(hits
            .into_iter()
            .map(|h| SearchHit {
                node_id: h.node_id,
                score: h.score,
                properties: HashMap::new(),
            })
            .collect())
    }

    async fn hybrid_search(&self, req: HybridSearchParams) -> Result<Vec<SearchHit>, GqlError> {
        let service_req = grafeo_service::types::HybridSearchReq {
            database: req.graph.clone(),
            label: req.label,
            text_property: req.text_property,
            vector_property: req.vector_property,
            query_text: req.query_text,
            query_vector: req.query_vector,
            k: req.k,
        };

        let hits = SearchService::hybrid_search(self.state.databases(), &req.graph, service_req)
            .await
            .map_err(|e| GqlError::Session(e.to_string()))?;

        Ok(hits
            .into_iter()
            .map(|h| SearchHit {
                node_id: h.node_id,
                score: h.score,
                properties: HashMap::new(),
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// ResultStream: lazily converts QueryResult into GWP streaming frames
// ---------------------------------------------------------------------------

use grafeo_service::stream::DEFAULT_BATCH_SIZE;

/// Phase of the lazy streaming state machine.
enum StreamPhase {
    /// Next poll yields the header frame.
    Header,
    /// Next poll encodes rows starting at `offset` into a Batch frame.
    Rows { offset: usize },
    /// Next poll yields the summary frame.
    Summary,
    /// Stream exhausted.
    Done,
}

/// A `ResultStream` that lazily yields frames from a `QueryResult`.
///
/// Instead of pre-building all frames in a `Vec`, this encodes rows
/// in batches of `DEFAULT_BATCH_SIZE`, reducing peak memory for the
/// encoded output and producing multiple Batch frames for large results.
struct GrafeoResultStream {
    result: grafeo_engine::database::QueryResult,
    batch_size: usize,
    phase: StreamPhase,
}

impl GrafeoResultStream {
    fn from_query_result(result: grafeo_engine::database::QueryResult) -> Self {
        Self {
            result,
            batch_size: DEFAULT_BATCH_SIZE,
            phase: StreamPhase::Header,
        }
    }
}

impl ResultStream for GrafeoResultStream {
    fn poll_next(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<ResultFrame, GqlError>>> {
        match self.phase {
            StreamPhase::Header => {
                let columns: Vec<proto::ColumnDescriptor> = self
                    .result
                    .columns
                    .iter()
                    .map(|name| proto::ColumnDescriptor {
                        name: name.clone(),
                        r#type: Some(proto::TypeDescriptor {
                            r#type: proto::GqlType::TypeAny.into(),
                            nullable: true,
                            element_type: None,
                            fields: Vec::new(),
                            precision: None,
                            scale: None,
                            min_length: None,
                            max_length: None,
                            max_cardinality: None,
                            is_group: false,
                            is_open: false,
                            duration_qualifier: proto::DurationQualifier::DurationUnspecified
                                .into(),
                            component_types: Vec::new(),
                        }),
                    })
                    .collect();

                let has_data = !self.result.rows().is_empty() || !columns.is_empty();

                let header = ResultFrame::Header(proto::ResultHeader {
                    result_type: if has_data {
                        proto::ResultType::BindingTable.into()
                    } else {
                        proto::ResultType::Omitted.into()
                    },
                    columns,
                    ordered: false,
                });

                self.phase = if self.result.rows().is_empty() {
                    StreamPhase::Summary
                } else {
                    StreamPhase::Rows { offset: 0 }
                };

                Poll::Ready(Some(Ok(header)))
            }

            StreamPhase::Rows { offset } => {
                let end = (offset + self.batch_size).min(self.result.rows().len());
                let rows: Vec<proto::Row> = self.result.rows()[offset..end]
                    .iter()
                    .map(|row| proto::Row {
                        values: row
                            .iter()
                            .map(|v| proto::Value::from(grafeo_to_gwp(v)))
                            .collect(),
                    })
                    .collect();

                let frame = ResultFrame::Batch(proto::RowBatch { rows });

                self.phase = if end >= self.result.rows().len() {
                    StreamPhase::Summary
                } else {
                    StreamPhase::Rows { offset: end }
                };

                Poll::Ready(Some(Ok(frame)))
            }

            StreamPhase::Summary => {
                let mut counters = HashMap::new();
                if let Some(ms) = self.result.execution_time_ms {
                    counters.insert("execution_time_ms".to_owned(), (ms * 1000.0) as i64);
                }
                if let Some(scanned) = self.result.rows_scanned {
                    counters.insert("rows_scanned".to_owned(), scanned as i64);
                }
                if let Some(written) =
                    grafeo_service::types::WriteCountersInfo::from_result(&self.result)
                {
                    for (name, value) in written.non_zero() {
                        counters.insert(
                            name.to_owned(),
                            grafeo_service::types::saturating_i64(value),
                        );
                    }
                }

                let summary = ResultFrame::Summary(proto::ResultSummary {
                    status: Some(status::success()),
                    warnings: Vec::new(),
                    rows_affected: self.result.rows().len() as i64,
                    counters,
                });

                self.phase = StreamPhase::Done;
                Poll::Ready(Some(Ok(summary)))
            }

            StreamPhase::Done => Poll::Ready(None),
        }
    }
}

/// GQLSTATUS 40001, serialization failure: the transaction conflicted with a
/// concurrent one and was rolled back; the client can run it again.
const SERIALIZATION_FAILURE: &str = "40001";

/// Maps a service error to a GWP error: a conflict with a concurrent
/// transaction is GQLSTATUS 40001 (serialization failure), which clients
/// retry; anything else gets `other`, with the message (42001 for a statement,
/// a transaction error for a commit, as before).
/// Where a catalog call works: a database and the identity to act as.
struct CatalogTarget {
    database: String,
    #[cfg(feature = "auth")]
    identity: Option<grafeo_engine::auth::Identity>,
}

/// The engine schema a catalog schema name means: `None` for the implicit
/// default schema (`""` or `"default"`).
fn schema_name(schema: &str) -> Option<&str> {
    (!schema.is_empty() && schema != DEFAULT_SCHEMA).then_some(schema)
}

/// A GQL delimited identifier: backticks, with a backtick doubled.
fn quote(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// Refuses the settings of a database in a catalog `CreateGraph`: the
/// catalog makes graphs inside the session's database, and databases are
/// created over HTTP (`POST /db`).
#[allow(clippy::result_large_err)]
fn refuse_database_options(config: &CreateGraphConfig) -> Result<(), GqlError> {
    let in_memory = matches!(
        config.storage_mode.to_lowercase().as_str(),
        "" | "inmemory" | "in-memory" | "in_memory"
    );
    if !in_memory
        || config.memory_limit_bytes.is_some()
        || config.backward_edges.is_some()
        || config.threads.is_some()
        || config.wal_enabled.is_some()
        || config.wal_durability.is_some()
    {
        return Err(GqlError::Session(
            "storage mode, memory limit, backward edges, threads and WAL settings belong to a \
             database: create databases over HTTP (POST /db) and pick one with the `_database` \
             session parameter"
                .to_owned(),
        ));
    }
    Ok(())
}

fn gql_error(error: ServiceError, other: impl FnOnce(String) -> GqlError) -> GqlError {
    match error {
        ServiceError::TransactionConflict(message) => {
            GqlError::status(SERIALIZATION_FAILURE, message)
        }
        error => other(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafeo_common::Value;
    use grafeo_common::types::LogicalType;
    use grafeo_engine::database::QueryResult;
    use std::future::poll_fn;
    use std::pin::Pin;

    fn make_result(num_rows: usize) -> QueryResult {
        let rows = (0..num_rows)
            .map(|i| vec![Value::Int64(i as i64)])
            .collect();
        let mut result = QueryResult::from_rows(vec!["x".to_string()], rows)
            .unwrap()
            .with_metrics(1.0, num_rows as u64);
        result.column_types = vec![LogicalType::Int64];
        result
    }

    /// Collects all frames from a `GrafeoResultStream`.
    async fn collect_frames(mut stream: Pin<Box<dyn ResultStream>>) -> Vec<ResultFrame> {
        let mut frames = Vec::new();
        loop {
            let frame = poll_fn(|cx| stream.as_mut().poll_next(cx)).await;
            match frame {
                Some(Ok(f)) => frames.push(f),
                Some(Err(e)) => panic!("unexpected error: {e:?}"),
                None => break,
            }
        }
        frames
    }

    #[tokio::test]
    async fn empty_result_yields_header_and_summary() {
        let result = make_result(0);
        let stream = Box::pin(GrafeoResultStream::from_query_result(result));
        let frames = collect_frames(stream).await;
        // Header + Summary (no Batch frames)
        assert_eq!(frames.len(), 2);
        assert!(matches!(frames[0], ResultFrame::Header(_)));
        assert!(matches!(frames[1], ResultFrame::Summary(_)));
    }

    #[tokio::test]
    async fn small_result_yields_single_batch() {
        let result = make_result(5);
        let stream = Box::pin(GrafeoResultStream::from_query_result(result));
        let frames = collect_frames(stream).await;
        // Header + 1 Batch + Summary
        assert_eq!(frames.len(), 3);
        assert!(matches!(frames[0], ResultFrame::Header(_)));
        if let ResultFrame::Batch(ref batch) = frames[1] {
            assert_eq!(batch.rows.len(), 5);
        } else {
            panic!("expected Batch frame");
        }
        assert!(matches!(frames[2], ResultFrame::Summary(_)));
    }

    #[tokio::test]
    async fn large_result_yields_multiple_batches() {
        let result = make_result(2500);
        let mut stream = GrafeoResultStream::from_query_result(result);
        stream.batch_size = 1000;
        let stream = Box::pin(stream);
        let frames = collect_frames(stream).await;
        // Header + 3 Batch (1000+1000+500) + Summary = 5
        assert_eq!(frames.len(), 5);
        assert!(matches!(frames[0], ResultFrame::Header(_)));
        if let ResultFrame::Batch(ref b) = frames[1] {
            assert_eq!(b.rows.len(), 1000);
        } else {
            panic!("expected Batch");
        }
        if let ResultFrame::Batch(ref b) = frames[2] {
            assert_eq!(b.rows.len(), 1000);
        } else {
            panic!("expected Batch");
        }
        if let ResultFrame::Batch(ref b) = frames[3] {
            assert_eq!(b.rows.len(), 500);
        } else {
            panic!("expected Batch");
        }
        assert!(matches!(frames[4], ResultFrame::Summary(_)));
    }
}
