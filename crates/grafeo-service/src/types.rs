//! Transport-agnostic types shared across the service layer.
//!
//! These types are used by `DatabaseManager`, `schema`, and transport
//! adapters. No HTTP or gRPC dependencies.

use serde::{Deserialize, Serialize};

/// Request to create a new named database.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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

#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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

// --- Output types ---

/// Summary info returned by the list endpoint.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DatabaseSummary {
    /// Database name.
    pub name: String,
    /// Number of nodes.
    pub node_count: usize,
    /// Number of edges.
    pub edge_count: usize,
    /// Whether the database uses persistent storage.
    pub persistent: bool,
    /// Database type: "lpg", "rdf", "owl-schema", "rdfs-schema", "json-schema".
    pub database_type: String,
}

/// Detailed info about a single database.
#[derive(Debug, Clone, Serialize)]
pub struct DatabaseInfo {
    pub name: String,
    pub node_count: usize,
    pub edge_count: usize,
    pub persistent: bool,
    pub version: String,
    pub wal_enabled: bool,
    pub database_type: String,
    pub storage_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_limit_bytes: Option<usize>,
    pub backward_edges: bool,
    pub threads: usize,
}

/// Database statistics.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DatabaseStats {
    pub name: String,
    pub node_count: usize,
    pub edge_count: usize,
    pub label_count: usize,
    pub edge_type_count: usize,
    pub property_key_count: usize,
    pub index_count: usize,
    pub memory_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_bytes: Option<usize>,
}

/// Schema information for a database.
#[derive(Debug, Clone, Serialize)]
pub struct SchemaInfo {
    pub name: String,
    pub labels: Vec<LabelInfo>,
    pub edge_types: Vec<EdgeTypeInfo>,
    pub property_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LabelInfo {
    pub name: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct EdgeTypeInfo {
    pub name: String,
    pub count: usize,
}

/// Health/status information.
#[derive(Debug, Clone, Serialize)]
pub struct HealthInfo {
    pub version: String,
    pub engine_version: String,
    pub persistent: bool,
    pub uptime_seconds: u64,
    pub active_sessions: usize,
    pub enabled_languages: Vec<String>,
    pub enabled_engine_features: Vec<String>,
    pub enabled_server_features: Vec<String>,
}

/// Compiled feature flags detected at build time.
///
/// Populated by the binary crate (which has all feature flags) and passed
/// to transport crates for health/status endpoints.
#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EnabledFeatures {
    /// Query language support (e.g. "gql", "cypher", "sparql").
    pub languages: Vec<String>,
    /// Engine capabilities (e.g. "parallel", "wal", "vector-index").
    pub engine: Vec<String>,
    /// Server capabilities (e.g. "auth", "tls", "gwp").
    pub server: Vec<String>,
}

/// Batch query input.
pub struct BatchQuery {
    pub statement: String,
    pub language: Option<String>,
    pub params: Option<std::collections::HashMap<String, grafeo_common::Value>>,
}

// ============================================================================
// Admin types
// ============================================================================

/// WAL (Write-Ahead Log) status information.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WalStatusInfo {
    /// Whether WAL is enabled for this database.
    pub enabled: bool,
    /// WAL file path (if persistent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// WAL size in bytes.
    pub size_bytes: usize,
    /// Number of WAL records.
    pub record_count: usize,
    /// Last checkpoint timestamp (Unix epoch seconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_checkpoint: Option<u64>,
    /// Current epoch/LSN.
    pub current_epoch: u64,
}

/// Database validation result.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ValidationInfo {
    /// Whether the database passed validation (no errors).
    pub valid: bool,
    /// Validation errors.
    pub errors: Vec<ValidationErrorItem>,
    /// Validation warnings.
    pub warnings: Vec<ValidationWarningItem>,
}

/// A validation error.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ValidationErrorItem {
    /// Error code (e.g. "DANGLING_SRC").
    pub code: String,
    /// Human-readable error message.
    pub message: String,
    /// Optional context (e.g. affected entity ID).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

/// A validation warning.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ValidationWarningItem {
    /// Warning code (e.g. "NO_EDGES").
    pub code: String,
    /// Human-readable warning message.
    pub message: String,
    /// Optional context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

/// Index definition for create/drop operations.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IndexDef {
    /// Property hash index for O(1) equality lookups.
    Property {
        property: String,
    },
    /// Vector similarity index (HNSW).
    Vector {
        label: String,
        property: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        dimensions: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        metric: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        m: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ef_construction: Option<u32>,
    },
    /// Full-text index (BM25).
    Text {
        label: String,
        property: String,
    },
}

// ============================================================================
// Search types
// ============================================================================

/// Vector search request parameters.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VectorSearchReq {
    /// Database name (defaults to "default").
    #[serde(default = "default_db_name")]
    pub database: String,
    /// Node label to search within.
    pub label: String,
    /// Property containing vector embeddings.
    pub property: String,
    /// Query vector.
    pub query_vector: Vec<f32>,
    /// Number of nearest neighbors to return.
    pub k: u32,
    /// Search beam width (higher = better recall).
    #[serde(default)]
    pub ef: Option<u32>,
    /// Optional property equality filters.
    #[serde(default)]
    pub filters: std::collections::HashMap<String, grafeo_common::Value>,
}

/// Text search request parameters.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TextSearchReq {
    /// Database name (defaults to "default").
    #[serde(default = "default_db_name")]
    pub database: String,
    /// Node label to search within.
    pub label: String,
    /// Property indexed for text search.
    pub property: String,
    /// Search query text.
    pub query: String,
    /// Number of results to return.
    pub k: u32,
}

/// Hybrid search request parameters (vector + text).
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct HybridSearchReq {
    /// Database name (defaults to "default").
    #[serde(default = "default_db_name")]
    pub database: String,
    /// Node label to search within.
    pub label: String,
    /// Property indexed for text search.
    pub text_property: String,
    /// Property indexed for vector search.
    pub vector_property: String,
    /// Text query for BM25 search.
    pub query_text: String,
    /// Vector query for similarity search (optional).
    #[serde(default)]
    pub query_vector: Vec<f32>,
    /// Number of results to return.
    pub k: u32,
}

/// A single search result hit.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SearchHit {
    /// Node identifier.
    pub node_id: u64,
    /// Relevance score (distance for vector, BM25 for text, fused for hybrid).
    pub score: f64,
    /// Node properties (empty by default, populated if requested).
    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub properties: std::collections::HashMap<String, serde_json::Value>,
}

fn default_db_name() -> String {
    "default".to_owned()
}
