//! `GrafeoBackend` — implements `boltr::server::BoltBackend` for Grafeo.

use std::collections::HashMap;
use std::sync::Arc;

use boltr::error::BoltError;
use boltr::server::{
    BoltBackend, BoltRecord, ResultMetadata, ResultStream, SessionConfig, SessionHandle,
    SessionProperty, TransactionHandle,
};
use boltr::types::{BoltDict, BoltValue};
use dashmap::DashMap;
use parking_lot::Mutex;
use uuid::Uuid;

use grafeo_service::ServiceState;
use grafeo_service::query::QueryService;

use crate::encode::{convert_params, grafeo_to_bolt};

struct GrafeoSession {
    engine_session: grafeo_engine::Session,
    database: String,
}

/// Bolt backend implementation for Grafeo.
pub struct GrafeoBackend {
    state: ServiceState,
    sessions: DashMap<String, Arc<Mutex<GrafeoSession>>>,
}

impl GrafeoBackend {
    pub fn new(state: ServiceState) -> Self {
        Self {
            state,
            sessions: DashMap::new(),
        }
    }

    fn get_session(
        &self,
        handle: &SessionHandle,
    ) -> Result<Arc<Mutex<GrafeoSession>>, BoltError> {
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

        let engine_session = tokio::task::spawn_blocking(move || entry.db.session())
            .await
            .map_err(BoltError::backend)?;

        let id = Uuid::new_v4().to_string();
        self.sessions.insert(
            id.clone(),
            Arc::new(Mutex::new(GrafeoSession {
                engine_session,
                database: "default".to_owned(),
            })),
        );
        tracing::debug!(session_id = %id, "Bolt session created");
        Ok(SessionHandle(id))
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
                let entry = self
                    .state
                    .databases()
                    .get(&db_name)
                    .ok_or_else(|| BoltError::Session(format!("database '{db_name}' not found")))?;

                let engine_session = tokio::task::spawn_blocking(move || entry.db.session())
                    .await
                    .map_err(BoltError::backend)?;

                let session_arc = self.get_session(session)?;
                let mut s = session_arc.lock();
                s.engine_session = engine_session;
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

        let engine_session = tokio::task::spawn_blocking(move || entry.db.session())
            .await
            .map_err(BoltError::backend)?;

        let session_arc = self.get_session(session)?;
        let mut s = session_arc.lock();
        s.engine_session = engine_session;
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
        let statement = query.to_owned();
        let params = convert_params(parameters);

        // Grafeo extension: language selection via extra dict.
        let language = extra
            .get("language")
            .and_then(|v| v.as_str())
            .map(String::from);

        let result = tokio::task::spawn_blocking(move || {
            let session = session_arc.lock();
            let params_opt = if params.is_empty() {
                None
            } else {
                Some(&params)
            };
            QueryService::dispatch(
                &session.engine_session,
                &statement,
                language.as_deref(),
                params_opt,
            )
        })
        .await
        .map_err(BoltError::backend)?
        .map_err(|e| BoltError::Query {
            code: "Neo.ClientError.Statement.SyntaxError".to_string(),
            message: e.to_string(),
        })?;

        let columns = result.columns.clone();
        let records: Vec<BoltRecord> = result
            .rows
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
        summary.insert("type".to_string(), BoltValue::String("r".to_string()));

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
            s.engine_session.begin_tx()
        })
        .await
        .map_err(BoltError::backend)?
        .map_err(|e| BoltError::Transaction(e.to_string()))?;

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
            s.engine_session.commit()
        })
        .await
        .map_err(BoltError::backend)?
        .map_err(|e| BoltError::Transaction(e.to_string()))?;
        Ok(BoltDict::new())
    }

    async fn rollback(
        &self,
        session: &SessionHandle,
        _transaction: &TransactionHandle,
    ) -> Result<(), BoltError> {
        let session_arc = self.get_session(session)?;
        tokio::task::spawn_blocking(move || {
            let mut s = session_arc.lock();
            s.engine_session.rollback()
        })
        .await
        .map_err(BoltError::backend)?
        .map_err(|e| BoltError::Transaction(e.to_string()))
    }

    async fn get_server_info(&self) -> Result<BoltDict, BoltError> {
        Ok(BoltDict::from([(
            "server".to_string(),
            BoltValue::String(format!("GrafeoDB/{}", env!("CARGO_PKG_VERSION"))),
        )]))
    }
}
