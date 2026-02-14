//! GQL Wire Protocol backend — bridges `gwp` to `grafeo-engine`.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use dashmap::DashMap;
use gwp::error::GqlError;
use gwp::proto;
use gwp::server::{
    GqlBackend, ResetTarget, ResultFrame, ResultStream, SessionConfig, SessionHandle,
    SessionProperty, TransactionHandle,
};
use gwp::status;
use gwp::types::Value as GwpValue;
use parking_lot::Mutex;
use uuid::Uuid;

use crate::state::AppState;

/// A GWP session backed by a grafeo-engine `Session`.
struct GrafeoSession {
    engine_session: grafeo_engine::Session,
    database: String,
}

/// GQL Wire Protocol backend for Grafeo.
///
/// Implements `GqlBackend` by delegating to grafeo-engine sessions.
/// Each GWP session maps to one engine session on a specific database.
/// All engine operations run via `spawn_blocking` to avoid blocking
/// the async runtime.
pub struct GrafeoBackend {
    state: AppState,
    sessions: DashMap<String, Arc<Mutex<GrafeoSession>>>,
}

impl GrafeoBackend {
    /// Creates a new backend wrapping the shared application state.
    pub fn new(state: AppState) -> Self {
        Self {
            state,
            sessions: DashMap::new(),
        }
    }

    /// Looks up an internal session by handle.
    #[allow(clippy::result_large_err)]
    fn get_session(&self, handle: &SessionHandle) -> Result<Arc<Mutex<GrafeoSession>>, GqlError> {
        self.sessions
            .get(&handle.0)
            .map(|entry| Arc::clone(entry.value()))
            .ok_or_else(|| GqlError::Session(format!("session '{}' not found", handle.0)))
    }
}

#[tonic::async_trait]
impl GqlBackend for GrafeoBackend {
    async fn create_session(&self, _config: &SessionConfig) -> Result<SessionHandle, GqlError> {
        let entry = self
            .state
            .databases()
            .get("default")
            .ok_or_else(|| GqlError::Session("default database not found".to_owned()))?;

        let engine_session = tokio::task::spawn_blocking(move || entry.db.session())
            .await
            .map_err(GqlError::backend)?;

        let id = Uuid::new_v4().to_string();
        self.sessions.insert(
            id.clone(),
            Arc::new(Mutex::new(GrafeoSession {
                engine_session,
                database: "default".to_owned(),
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
        match property {
            SessionProperty::Graph(db_name) => {
                let entry =
                    self.state.databases().get(&db_name).ok_or_else(|| {
                        GqlError::Session(format!("database '{db_name}' not found"))
                    })?;

                let engine_session = tokio::task::spawn_blocking(move || entry.db.session())
                    .await
                    .map_err(GqlError::backend)?;

                let session_arc = self.get_session(session)?;
                let mut s = session_arc.lock();
                s.engine_session = engine_session;
                s.database = db_name;
            }
            SessionProperty::Schema(_)
            | SessionProperty::TimeZone(_)
            | SessionProperty::Parameter { .. } => {}
        }
        Ok(())
    }

    async fn reset_session(
        &self,
        session: &SessionHandle,
        _target: ResetTarget,
    ) -> Result<(), GqlError> {
        let entry = self
            .state
            .databases()
            .get("default")
            .ok_or_else(|| GqlError::Session("default database not found".to_owned()))?;

        let engine_session = tokio::task::spawn_blocking(move || entry.db.session())
            .await
            .map_err(GqlError::backend)?;

        let session_arc = self.get_session(session)?;
        let mut s = session_arc.lock();
        s.engine_session = engine_session;
        "default".clone_into(&mut s.database);
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
            if params.is_empty() {
                session.engine_session.execute(&statement)
            } else {
                session
                    .engine_session
                    .execute_with_params(&statement, params)
            }
        })
        .await
        .map_err(GqlError::backend)?
        .map_err(|e| GqlError::status(status::INVALID_SYNTAX, e.to_string()))?;

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
            s.engine_session.begin_tx()
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
        .map_err(|e| GqlError::Transaction(e.to_string()))
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
}

// ---------------------------------------------------------------------------
// Value conversion: grafeo_common::Value <-> gwp::Value
// ---------------------------------------------------------------------------

/// Converts a grafeo-engine `Value` to a GWP `Value`.
fn grafeo_to_gwp(value: &grafeo_common::Value) -> GwpValue {
    use grafeo_common::Value;
    match value {
        Value::Null => GwpValue::Null,
        Value::Bool(b) => GwpValue::Boolean(*b),
        Value::Int64(i) => GwpValue::Integer(*i),
        Value::Float64(f) => GwpValue::Float(*f),
        Value::String(s) => GwpValue::String(s.to_string()),
        Value::Bytes(b) => GwpValue::Bytes(b.to_vec()),
        Value::Timestamp(t) => GwpValue::String(format!("{t:?}")),
        Value::List(items) => GwpValue::List(items.iter().map(grafeo_to_gwp).collect()),
        Value::Map(map) => {
            let fields: Vec<gwp::types::Field> = map
                .iter()
                .map(|(k, v)| gwp::types::Field {
                    name: k.to_string(),
                    value: grafeo_to_gwp(v),
                })
                .collect();
            GwpValue::Record(gwp::types::Record { fields })
        }
        Value::Vector(v) => {
            GwpValue::List(v.iter().map(|f| GwpValue::Float(f64::from(*f))).collect())
        }
    }
}

/// Converts GWP parameters to grafeo-engine parameters.
fn convert_params(params: &HashMap<String, GwpValue>) -> HashMap<String, grafeo_common::Value> {
    params
        .iter()
        .filter_map(|(k, v)| gwp_to_grafeo(v).map(|gv| (k.clone(), gv)))
        .collect()
}

/// Converts a GWP `Value` to a grafeo-engine `Value`.
/// Returns None for types that grafeo-engine doesn't support as parameters.
fn gwp_to_grafeo(value: &GwpValue) -> Option<grafeo_common::Value> {
    use grafeo_common::Value;
    match value {
        GwpValue::Null => Some(Value::Null),
        GwpValue::Boolean(b) => Some(Value::Bool(*b)),
        GwpValue::Integer(i) => Some(Value::Int64(*i)),
        GwpValue::UnsignedInteger(u) => Some(Value::Int64(*u as i64)),
        GwpValue::Float(f) => Some(Value::Float64(*f)),
        GwpValue::String(s) => Some(Value::String(s.as_str().into())),
        GwpValue::Bytes(b) => Some(Value::Bytes(b.clone().into())),
        GwpValue::List(items) => {
            let converted: Vec<_> = items.iter().filter_map(gwp_to_grafeo).collect();
            Some(Value::List(converted.into()))
        }
        // Temporal and graph types: not supported as engine parameters
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// ResultStream: converts QueryResult into GWP streaming frames
// ---------------------------------------------------------------------------

/// A `ResultStream` that yields pre-built frames from a `QueryResult`.
struct GrafeoResultStream {
    frames: Vec<ResultFrame>,
    index: usize,
}

impl GrafeoResultStream {
    fn from_query_result(result: grafeo_engine::database::QueryResult) -> Self {
        let columns: Vec<proto::ColumnDescriptor> = result
            .columns
            .iter()
            .map(|name| proto::ColumnDescriptor {
                name: name.clone(),
                r#type: Some(proto::TypeDescriptor {
                    r#type: proto::GqlType::TypeAny.into(),
                    nullable: true,
                    element_type: None,
                    fields: Vec::new(),
                }),
            })
            .collect();

        let has_rows = !result.rows.is_empty();

        let header = ResultFrame::Header(proto::ResultHeader {
            result_type: if has_rows || !columns.is_empty() {
                proto::ResultType::BindingTable.into()
            } else {
                proto::ResultType::Omitted.into()
            },
            columns,
        });

        let mut frames = vec![header];

        if has_rows {
            let rows: Vec<proto::Row> = result
                .rows
                .iter()
                .map(|row| proto::Row {
                    values: row
                        .iter()
                        .map(|v| proto::Value::from(grafeo_to_gwp(v)))
                        .collect(),
                })
                .collect();
            frames.push(ResultFrame::Batch(proto::RowBatch { rows }));
        }

        let mut counters = HashMap::new();
        if let Some(ms) = result.execution_time_ms {
            counters.insert("execution_time_ms".to_owned(), (ms * 1000.0) as i64);
        }
        if let Some(scanned) = result.rows_scanned {
            counters.insert("rows_scanned".to_owned(), scanned as i64);
        }

        frames.push(ResultFrame::Summary(proto::ResultSummary {
            status: Some(status::success()),
            warnings: Vec::new(),
            rows_affected: result.rows.len() as i64,
            counters,
        }));

        Self { frames, index: 0 }
    }
}

impl ResultStream for GrafeoResultStream {
    fn poll_next(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<ResultFrame, GqlError>>> {
        if self.index < self.frames.len() {
            let frame = self.frames[self.index].clone();
            self.index += 1;
            Poll::Ready(Some(Ok(frame)))
        } else {
            Poll::Ready(None)
        }
    }
}
