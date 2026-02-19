//! GqlBackend trait implementation for Grafeo.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use dashmap::DashMap;
use gwp::error::GqlError;
use gwp::proto;
use gwp::server::{
    AdminStats, AdminValidationResult, AdminWalStatus, CreateDatabaseConfig, DatabaseInfo,
    GqlBackend, HybridSearchParams, IndexDefinition, ResetTarget, ResultFrame, ResultStream,
    SearchHit, SessionConfig, SessionHandle, SessionProperty, TextSearchParams,
    TransactionHandle, ValidationDiagnostic, VectorSearchParams,
};
use gwp::status;
use gwp::types::Value as GwpValue;
use parking_lot::Mutex;
use uuid::Uuid;

use grafeo_service::ServiceState;
use grafeo_service::admin::AdminService;
use grafeo_service::search::SearchService;

use crate::encode::{convert_params, grafeo_to_gwp};

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
    state: ServiceState,
    sessions: DashMap<String, Arc<Mutex<GrafeoSession>>>,
}

impl GrafeoBackend {
    /// Creates a new backend wrapping the shared service state.
    pub fn new(state: ServiceState) -> Self {
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

    // -----------------------------------------------------------------
    // Database lifecycle
    // -----------------------------------------------------------------

    async fn list_databases(&self) -> Result<Vec<DatabaseInfo>, GqlError> {
        let list = self.state.databases().list();
        Ok(list
            .into_iter()
            .map(|s| DatabaseInfo {
                name: s.name,
                node_count: s.node_count as u64,
                edge_count: s.edge_count as u64,
                persistent: s.persistent,
                database_type: s.database_type,
                storage_mode: String::new(),
                memory_limit_bytes: None,
                backward_edges: None,
                threads: None,
            })
            .collect())
    }

    async fn create_database(
        &self,
        config: CreateDatabaseConfig,
    ) -> Result<DatabaseInfo, GqlError> {
        use grafeo_service::types::{
            CreateDatabaseRequest, DatabaseOptions, DatabaseType, StorageMode,
        };

        let database_type = match config.database_type.to_lowercase().as_str() {
            "lpg" => DatabaseType::Lpg,
            "rdf" => DatabaseType::Rdf,
            other => {
                return Err(GqlError::Session(format!(
                    "unsupported database type: {other}"
                )));
            }
        };

        let storage_mode = match config.storage_mode.to_lowercase().as_str() {
            "inmemory" | "in-memory" | "in_memory" | "" => StorageMode::InMemory,
            "persistent" => StorageMode::Persistent,
            other => {
                return Err(GqlError::Session(format!(
                    "unsupported storage mode: {other}"
                )));
            }
        };

        let req = CreateDatabaseRequest {
            name: config.name.clone(),
            database_type,
            storage_mode,
            options: DatabaseOptions {
                memory_limit_bytes: config.memory_limit_bytes.map(|v| v as usize),
                backward_edges: config.backward_edges,
                threads: config.threads.map(|v| v as usize),
                wal_enabled: config.wal_enabled,
                wal_durability: config.wal_durability,
                spill_path: None,
            },
            schema_file: None,
            schema_filename: None,
        };

        self.state
            .databases()
            .create(&req)
            .map_err(|e| GqlError::Session(e.to_string()))?;

        self.get_database_info(&config.name).await
    }

    async fn delete_database(&self, name: &str) -> Result<String, GqlError> {
        // Invalidate GWP sessions targeting this database before deleting
        let sessions_to_remove: Vec<String> = self
            .sessions
            .iter()
            .filter(|entry| entry.value().lock().database == name)
            .map(|entry| entry.key().clone())
            .collect();

        if !sessions_to_remove.is_empty() {
            tracing::info!(
                database = %name,
                count = sessions_to_remove.len(),
                "Invalidating GWP sessions for deleted database"
            );
            for sid in &sessions_to_remove {
                self.sessions.remove(sid);
            }
        }

        self.state
            .databases()
            .delete(name)
            .map_err(|e| GqlError::Session(e.to_string()))?;

        Ok(name.to_owned())
    }

    async fn get_database_info(&self, name: &str) -> Result<DatabaseInfo, GqlError> {
        let entry = self
            .state
            .databases()
            .get(name)
            .ok_or_else(|| GqlError::Session(format!("database '{name}' not found")))?;

        Ok(DatabaseInfo {
            name: name.to_owned(),
            node_count: entry.db.node_count() as u64,
            edge_count: entry.db.edge_count() as u64,
            persistent: entry.db.path().is_some(),
            database_type: entry.metadata.database_type.clone(),
            storage_mode: entry.metadata.storage_mode.clone(),
            memory_limit_bytes: entry.db.memory_limit().map(|v| v as u64),
            backward_edges: Some(entry.metadata.backward_edges),
            threads: Some(entry.metadata.threads as u32),
        })
    }

    // -----------------------------------------------------------------
    // Admin operations
    // -----------------------------------------------------------------

    async fn get_database_stats(&self, name: &str) -> Result<AdminStats, GqlError> {
        let stats = AdminService::database_stats(self.state.databases(), name)
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

    async fn wal_status(&self, name: &str) -> Result<AdminWalStatus, GqlError> {
        let status = AdminService::wal_status(self.state.databases(), name)
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

    async fn wal_checkpoint(&self, name: &str) -> Result<(), GqlError> {
        AdminService::wal_checkpoint(self.state.databases(), name)
            .await
            .map_err(|e| GqlError::Session(e.to_string()))
    }

    async fn validate(&self, name: &str) -> Result<AdminValidationResult, GqlError> {
        let result = AdminService::validate(self.state.databases(), name)
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

    async fn create_index(&self, name: &str, index: IndexDefinition) -> Result<(), GqlError> {
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

        AdminService::create_index(self.state.databases(), name, service_index)
            .await
            .map_err(|e| GqlError::Session(e.to_string()))
    }

    async fn drop_index(&self, name: &str, index: IndexDefinition) -> Result<bool, GqlError> {
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

        AdminService::drop_index(self.state.databases(), name, service_index)
            .await
            .map_err(|e| GqlError::Session(e.to_string()))
    }

    // -----------------------------------------------------------------
    // Search operations
    // -----------------------------------------------------------------

    async fn vector_search(&self, req: VectorSearchParams) -> Result<Vec<SearchHit>, GqlError> {
        let service_req = grafeo_service::types::VectorSearchReq {
            database: req.database.clone(),
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

        let hits = SearchService::vector_search(self.state.databases(), &req.database, service_req)
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
            database: req.database.clone(),
            label: req.label,
            property: req.property,
            query: req.query,
            k: req.k,
        };

        let hits = SearchService::text_search(self.state.databases(), &req.database, service_req)
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
            database: req.database.clone(),
            label: req.label,
            text_property: req.text_property,
            vector_property: req.vector_property,
            query_text: req.query_text,
            query_vector: req.query_vector,
            k: req.k,
        };

        let hits =
            SearchService::hybrid_search(self.state.databases(), &req.database, service_req)
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
                        }),
                    })
                    .collect();

                let has_data = !self.result.rows.is_empty() || !columns.is_empty();

                let header = ResultFrame::Header(proto::ResultHeader {
                    result_type: if has_data {
                        proto::ResultType::BindingTable.into()
                    } else {
                        proto::ResultType::Omitted.into()
                    },
                    columns,
                });

                self.phase = if self.result.rows.is_empty() {
                    StreamPhase::Summary
                } else {
                    StreamPhase::Rows { offset: 0 }
                };

                Poll::Ready(Some(Ok(header)))
            }

            StreamPhase::Rows { offset } => {
                let end = (offset + self.batch_size).min(self.result.rows.len());
                let rows: Vec<proto::Row> = self.result.rows[offset..end]
                    .iter()
                    .map(|row| proto::Row {
                        values: row
                            .iter()
                            .map(|v| proto::Value::from(grafeo_to_gwp(v)))
                            .collect(),
                    })
                    .collect();

                let frame = ResultFrame::Batch(proto::RowBatch { rows });

                self.phase = if end >= self.result.rows.len() {
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

                let summary = ResultFrame::Summary(proto::ResultSummary {
                    status: Some(status::success()),
                    warnings: Vec::new(),
                    rows_affected: self.result.rows.len() as i64,
                    counters,
                });

                self.phase = StreamPhase::Done;
                Poll::Ready(Some(Ok(summary)))
            }

            StreamPhase::Done => Poll::Ready(None),
        }
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
        QueryResult {
            columns: vec!["x".to_string()],
            column_types: vec![LogicalType::Int64],
            rows: (0..num_rows)
                .map(|i| vec![Value::Int64(i as i64)])
                .collect(),
            execution_time_ms: Some(1.0),
            rows_scanned: Some(num_rows as u64),
        }
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
