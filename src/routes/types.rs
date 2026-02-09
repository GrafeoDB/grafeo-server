//! Request/response types for the Grafeo Server HTTP API.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::database_manager::DatabaseSummary;

#[derive(Deserialize, ToSchema)]
pub struct QueryRequest {
    /// The query string to execute.
    pub query: String,
    /// Optional query parameters (JSON object).
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    /// Query language: "gql" (default), "cypher", "graphql", "gremlin", "sparql".
    /// Ignored by language-specific convenience endpoints.
    #[serde(default)]
    pub language: Option<String>,
    /// Target database name (defaults to "default").
    #[serde(default)]
    pub database: Option<String>,
    /// Per-query timeout override in milliseconds (0 = use server default).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Serialize, ToSchema)]
pub struct QueryResponse {
    /// Column names from the result set.
    pub columns: Vec<String>,
    /// Result rows, each containing JSON-encoded values.
    pub rows: Vec<Vec<serde_json::Value>>,
    /// Time taken to execute the query in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_time_ms: Option<f64>,
    /// Number of rows scanned during query execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows_scanned: Option<u64>,
}

#[derive(Deserialize, ToSchema)]
pub struct TxBeginRequest {
    /// Target database name (defaults to "default").
    #[serde(default)]
    pub database: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct TransactionResponse {
    /// Unique session identifier for the transaction.
    pub session_id: String,
    /// Transaction status: "open", "committed", or "rolled_back".
    pub status: String,
}

#[derive(Serialize, ToSchema)]
pub struct HealthResponse {
    /// Server status ("ok").
    pub status: String,
    /// Server version.
    pub version: String,
    /// Grafeo engine version.
    pub engine_version: String,
    /// Whether the server is using persistent storage.
    pub persistent: bool,
    /// Server uptime in seconds.
    pub uptime_seconds: u64,
    /// Number of active transaction sessions across all databases.
    pub active_sessions: usize,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateDatabaseRequest {
    /// Name for the new database.
    pub name: String,
    /// Database type: determines graph model and schema handling.
    #[serde(default)]
    pub database_type: DatabaseType,
    /// Storage mode: in-memory (default) or persistent.
    #[serde(default)]
    pub storage_mode: StorageMode,
    /// Resource and tuning options.
    #[serde(default)]
    pub options: DatabaseOptions,
    /// Base64-encoded schema file content (OWL/RDFS/JSON Schema).
    #[serde(default)]
    pub schema_file: Option<String>,
    /// Original filename for format detection.
    #[serde(default)]
    pub schema_filename: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema, Default, PartialEq, Eq)]
pub enum DatabaseType {
    /// Labeled Property Graph (default). Supports GQL, Cypher, Gremlin, GraphQL.
    #[default]
    Lpg,
    /// RDF triple store. Supports SPARQL.
    Rdf,
    /// RDF with OWL ontology loaded from schema file.
    OwlSchema,
    /// RDF with RDFS schema loaded from schema file.
    RdfsSchema,
    /// LPG with JSON Schema constraints.
    JsonSchema,
}

impl DatabaseType {
    /// Returns the engine GraphModel for this database type.
    pub fn graph_model(self) -> grafeo_engine::GraphModel {
        match self {
            Self::Lpg | Self::JsonSchema => grafeo_engine::GraphModel::Lpg,
            Self::Rdf | Self::OwlSchema | Self::RdfsSchema => grafeo_engine::GraphModel::Rdf,
        }
    }

    /// Whether this type requires a schema file upload.
    pub fn requires_schema_file(self) -> bool {
        matches!(self, Self::OwlSchema | Self::RdfsSchema | Self::JsonSchema)
    }

    /// Display name for API responses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lpg => "lpg",
            Self::Rdf => "rdf",
            Self::OwlSchema => "owl-schema",
            Self::RdfsSchema => "rdfs-schema",
            Self::JsonSchema => "json-schema",
        }
    }
}

impl std::fmt::Display for DatabaseType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema, Default, PartialEq, Eq)]
pub enum StorageMode {
    /// Fast, ephemeral storage. Data lost on restart.
    #[default]
    InMemory,
    /// WAL-backed durable storage. Requires --data-dir.
    Persistent,
}

impl StorageMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InMemory => "in-memory",
            Self::Persistent => "persistent",
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct DatabaseOptions {
    /// Memory limit in bytes. Default: 512 MB.
    #[serde(default)]
    pub memory_limit_bytes: Option<usize>,
    /// Enable write-ahead log. Default: true for persistent, false for in-memory.
    #[serde(default)]
    pub wal_enabled: Option<bool>,
    /// WAL durability mode: "sync", "batch" (default), "adaptive", "nosync".
    #[serde(default)]
    pub wal_durability: Option<String>,
    /// Maintain backward edges. Default: true. Disable to save ~50% adjacency memory.
    #[serde(default)]
    pub backward_edges: Option<bool>,
    /// Worker threads for query execution. Default: CPU count.
    #[serde(default)]
    pub threads: Option<usize>,
    /// Optional path for out-of-core spill processing.
    #[serde(default)]
    pub spill_path: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct SystemResources {
    /// Total system RAM in bytes.
    pub total_memory_bytes: u64,
    /// Memory already allocated to existing databases.
    pub allocated_memory_bytes: u64,
    /// Max available for a new DB (80% system RAM - allocated).
    pub available_memory_bytes: u64,
    /// Disk space available at data_dir partition (None if no data_dir).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_disk_bytes: Option<u64>,
    /// Whether persistent storage is available (data_dir is set).
    pub persistent_available: bool,
    /// Available database types based on compiled features.
    pub available_types: Vec<String>,
    /// Default values for database options.
    pub defaults: ResourceDefaults,
}

#[derive(Serialize, ToSchema)]
pub struct ResourceDefaults {
    /// Default memory limit in bytes (512 MB).
    pub memory_limit_bytes: u64,
    /// Default storage mode.
    pub storage_mode: String,
    /// Default WAL enabled state.
    pub wal_enabled: bool,
    /// Default WAL durability mode.
    pub wal_durability: String,
    /// Default backward edges setting.
    pub backward_edges: bool,
    /// Default thread count.
    pub threads: usize,
}

#[derive(Serialize, ToSchema)]
pub struct ListDatabasesResponse {
    /// List of all databases.
    pub databases: Vec<DatabaseSummary>,
}

#[derive(Serialize, ToSchema)]
pub struct DatabaseInfoResponse {
    /// Database name.
    pub name: String,
    /// Number of nodes.
    pub node_count: usize,
    /// Number of edges.
    pub edge_count: usize,
    /// Whether the database uses persistent storage.
    pub persistent: bool,
    /// Database version string from the engine.
    pub version: String,
    /// Whether WAL is enabled.
    pub wal_enabled: bool,
    /// Database type: "lpg", "rdf", "owl-schema", "rdfs-schema", "json-schema".
    pub database_type: String,
    /// Storage mode: "in-memory" or "persistent".
    pub storage_mode: String,
    /// Configured memory limit in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_limit_bytes: Option<usize>,
    /// Whether backward edges are enabled.
    pub backward_edges: bool,
    /// Number of worker threads.
    pub threads: usize,
}

#[derive(Serialize, ToSchema)]
pub struct DatabaseStatsResponse {
    /// Database name.
    pub name: String,
    /// Number of nodes.
    pub node_count: usize,
    /// Number of edges.
    pub edge_count: usize,
    /// Number of distinct labels.
    pub label_count: usize,
    /// Number of distinct edge types.
    pub edge_type_count: usize,
    /// Number of distinct property keys.
    pub property_key_count: usize,
    /// Number of indexes.
    pub index_count: usize,
    /// Approximate memory usage in bytes.
    pub memory_bytes: usize,
    /// Approximate disk usage in bytes (persistent only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_bytes: Option<usize>,
}

#[derive(Serialize, ToSchema)]
pub struct DatabaseSchemaResponse {
    /// Database name.
    pub name: String,
    /// Node labels with counts.
    pub labels: Vec<LabelInfo>,
    /// Edge types with counts.
    pub edge_types: Vec<EdgeTypeInfo>,
    /// Property key names.
    pub property_keys: Vec<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct BatchQueryRequest {
    /// Array of queries to execute sequentially in a single transaction.
    pub queries: Vec<BatchQueryItem>,
    /// Target database name (defaults to "default").
    #[serde(default)]
    pub database: Option<String>,
    /// Overall timeout in milliseconds (0 = use server default).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Deserialize, ToSchema)]
pub struct BatchQueryItem {
    /// The query string to execute.
    pub query: String,
    /// Query language: "gql" (default), "cypher", "graphql", "gremlin", "sparql".
    #[serde(default)]
    pub language: Option<String>,
    /// Optional query parameters (JSON object).
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

#[derive(Serialize, ToSchema)]
pub struct BatchQueryResponse {
    /// Results for each query, in order.
    pub results: Vec<QueryResponse>,
    /// Total execution time in milliseconds.
    pub total_execution_time_ms: f64,
}

// ---------------------------------------------------------------------------
// WebSocket message types
// ---------------------------------------------------------------------------

/// Client-to-server WebSocket message.
#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum WsClientMessage {
    /// Execute a query (auto-commit).
    #[serde(rename = "query")]
    Query {
        /// Optional client-assigned ID for correlating responses.
        #[serde(default)]
        id: Option<String>,
        /// The query request payload.
        #[serde(flatten)]
        request: QueryRequest,
    },
    /// Application-level keepalive.
    #[serde(rename = "ping")]
    Ping,
}

/// Server-to-client WebSocket message.
#[derive(Serialize)]
#[serde(tag = "type")]
pub enum WsServerMessage {
    /// Query result.
    #[serde(rename = "result")]
    Result {
        /// Echoed from the client message, if provided.
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// The query response payload.
        #[serde(flatten)]
        response: QueryResponse,
    },
    /// Query error.
    #[serde(rename = "error")]
    Error {
        /// Echoed from the client message, if provided.
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Error code (e.g. "bad_request", "not_found", "timeout").
        error: String,
        /// Human-readable error detail.
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Pong response to a client ping.
    #[serde(rename = "pong")]
    Pong,
}

#[derive(Serialize, ToSchema)]
pub struct LabelInfo {
    /// Label name.
    pub name: String,
    /// Number of nodes with this label.
    pub count: usize,
}

#[derive(Serialize, ToSchema)]
pub struct EdgeTypeInfo {
    /// Edge type name.
    pub name: String,
    /// Number of edges with this type.
    pub count: usize,
}
