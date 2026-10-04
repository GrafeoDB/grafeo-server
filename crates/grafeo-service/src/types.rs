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

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
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
    /// Per-section storage tier overrides applied at db open (engine 0.5.42).
    ///
    /// Map of section name to tier override string. Recognised section names:
    /// `LpgStore`, `RdfStore`, `CompactStore`, `VectorStore`, `TextIndex`,
    /// `RdfRing`, `PropertyIndex`, `Catalog`. Recognised tier values:
    /// `auto` (default), `force_ram`, `force_disk`.
    #[serde(default)]
    pub section_tiers: Option<std::collections::HashMap<String, String>>,
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
    pub read_only: bool,
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
    Property { property: String },
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
    Text { label: String, property: String },
}

/// Query plan cache statistics.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CacheStatsInfo {
    /// Number of entries in the parsed query cache.
    pub parsed_size: usize,
    /// Number of entries in the optimized query cache.
    pub optimized_size: usize,
    /// Parsed cache hit count.
    pub parsed_hits: u64,
    /// Parsed cache miss count.
    pub parsed_misses: u64,
    /// Optimized cache hit count.
    pub optimized_hits: u64,
    /// Optimized cache miss count.
    pub optimized_misses: u64,
    /// Number of cache invalidations (DDL-triggered clears).
    pub invalidations: u64,
    /// Parsed cache hit rate (0.0 to 1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed_hit_rate: Option<f64>,
    /// Optimized cache hit rate (0.0 to 1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optimized_hit_rate: Option<f64>,
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

// ============================================================================
// Backup types
// ============================================================================

/// Information about a single backup segment.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BackupEntry {
    /// Backup filename.
    pub filename: String,
    /// Database this backup belongs to.
    pub database: String,
    /// Segment kind: "full" or "incremental".
    pub kind: String,
    /// Backup file size in bytes.
    pub size_bytes: u64,
    /// Backup creation timestamp (ISO 8601).
    pub created_at: String,
    /// Start epoch (inclusive).
    pub start_epoch: u64,
    /// End epoch (inclusive).
    pub end_epoch: u64,
    /// CRC-32 checksum of the backup file.
    pub checksum: u32,
    /// Optional user-supplied label (stored in a sidecar file, not the filename).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Request body for creating a backup.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateBackupRequest {
    /// Optional label for the backup. Must match `^[A-Za-z0-9_-]{1,32}$` when set.
    /// Stored in a sidecar file alongside the backup so filenames stay
    /// engine-controlled.
    #[serde(default)]
    pub label: Option<String>,
}

/// Request to restore a database from a backup.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RestoreRequest {
    /// Backup filename (within the source database's backup subdirectory).
    pub backup: String,
    /// Source database whose backup subdirectory holds the file.
    /// Defaults to the target database path parameter, which is the
    /// same-database restore case. Set to a different value for cross-
    /// database restores (e.g. restore prod's snapshot into staging).
    #[serde(default)]
    pub source_db: Option<String>,
}

/// Request to restore a database to a specific epoch.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RestoreToEpochRequest {
    /// Target epoch to restore to.
    pub epoch: u64,
}

// ============================================================================
// Token management types
// ============================================================================

/// Request to create a new API token.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateTokenRequest {
    /// Human-readable name for the token.
    pub name: String,
    /// Permission scope.
    #[serde(default)]
    pub scope: TokenScopeRequest,
    /// Token lifetime in seconds. If set, the token will expire after this
    /// duration. Omit for a non-expiring token.
    #[serde(default)]
    pub expires_in: Option<u64>,
}

/// Scope definition in a create/update request.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TokenScopeRequest {
    /// Permission level: "admin", "read-write", "read-only".
    #[serde(default = "default_role")]
    pub role: String,
    /// Databases this token can access. Empty = all databases.
    #[serde(default)]
    pub databases: Vec<String>,
}

impl Default for TokenScopeRequest {
    fn default() -> Self {
        Self {
            role: "read-only".to_string(),
            databases: vec![],
        }
    }
}

impl TokenScopeRequest {
    /// Parse the wire-format role string into the engine's [`Role`] enum.
    pub fn to_role(&self) -> Result<grafeo_engine::auth::Role, crate::error::ServiceError> {
        crate::auth::str_to_role(&self.role).map_err(crate::error::ServiceError::BadRequest)
    }
}

fn default_role() -> String {
    "read-only".to_string()
}

/// Token response (returned from list/get/create endpoints).
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TokenResponse {
    pub id: String,
    pub name: String,
    pub scope: TokenScopeRequest,
    pub created_at: String,
    /// Unix timestamp when the token expires. `null` if non-expiring.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// The plaintext token. Only present in the create response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

// ============================================================================
// Named graph types
// ============================================================================

/// Request to create a named graph within a database.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateGraphRequest {
    /// Name for the new graph.
    pub name: String,
}

/// Response for listing named graphs in a database.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GraphListResponse {
    /// Named graphs within the database.
    pub graphs: Vec<String>,
}

// ============================================================================
// Schema management types (ISO/IEC 39075 Section 4.2.5)
// ============================================================================

/// Response for listing schema namespaces.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SchemaListResponse {
    /// Schema namespace names.
    pub schemas: Vec<String>,
}

/// Request to create a schema namespace.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateSchemaRequest {
    /// Name for the new schema namespace.
    pub name: String,
}

// ============================================================================
// Graph projection types
// ============================================================================

/// Request to create a graph projection.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateProjectionRequest {
    /// Name for the projection.
    pub name: String,
    /// Node labels to include (empty = all nodes).
    #[serde(default)]
    pub node_labels: Vec<String>,
    /// Edge types to include (empty = all edges).
    #[serde(default)]
    pub edge_types: Vec<String>,
}

/// Response for listing graph projections.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ProjectionListResponse {
    /// Named projections in the database.
    pub projections: Vec<String>,
}

// ============================================================================
// Bulk import types
// ============================================================================

/// Request to bulk-import a TSV edge list into a database.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ImportTsvRequest {
    /// Database to import into.
    #[serde(default = "default_db_name")]
    pub database: String,
    /// Edge type label for all imported edges.
    #[serde(default = "default_edge_type")]
    pub edge_type: String,
    /// If true, create one directed edge per line. If false, create edges
    /// in both directions.
    #[serde(default = "default_true")]
    pub directed: bool,
    /// Tab or space-separated edge list data. Each line: `src_id dst_id`.
    /// Lines starting with `#` or `%` are comments.
    pub data: String,
}

fn default_edge_type() -> String {
    "EDGE".to_owned()
}

fn default_true() -> bool {
    true
}

/// Response from a bulk import operation.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ImportResponse {
    /// Number of nodes created.
    pub nodes_created: usize,
    /// Number of edges created.
    pub edges_created: usize,
}

// ============================================================================
// SHACL validation types
// ============================================================================

/// Request to validate RDF data against SHACL shapes.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ShaclValidateRequest {
    /// SHACL shapes graph (Turtle syntax).
    pub shapes_graph: String,
    /// Named data graph to validate. If omitted, validates the default graph.
    #[serde(default)]
    pub data_graph: Option<String>,
}

/// SHACL validation report.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ShaclValidationReport {
    /// Whether the data conforms to all shapes.
    pub conforms: bool,
    /// Individual constraint violations.
    pub results: Vec<ShaclViolation>,
}

/// A single SHACL constraint violation.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ShaclViolation {
    /// The focus node that violated the constraint.
    pub focus_node: String,
    /// The constraint component that was violated.
    pub constraint: String,
    /// The source shape that defined the constraint.
    pub source_shape: String,
    /// Severity level: "Violation", "Warning", or "Info".
    pub severity: String,
    /// The value that caused the violation, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// The property path, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Human-readable message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// ============================================================================
// Storage tier types (engine 0.5.42)
// ============================================================================

/// One section's current storage tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SectionTierInfo {
    /// Section name (e.g. `"LpgStore"`, `"VectorStore"`, `"CompactStore"`).
    pub section: String,
    /// Current tier (`"in_memory"`, `"on_disk"`, `"uninitialized"`).
    pub tier: String,
}

/// Response for `GET /admin/{db}/storage-tiers`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct StorageTiersResponse {
    pub tiers: Vec<SectionTierInfo>,
}

/// Request for `POST /admin/{db}/reload-eligible`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReloadEligibleRequest {
    /// Target fraction of memory budget to occupy after reload, in `[0.0, 1.0]`.
    /// Default: `0.7`.
    #[serde(default)]
    pub target_fraction: Option<f64>,
}

/// Response for `POST /admin/{db}/reload-eligible`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReloadEligibleResponse {
    /// Number of sections that were reloaded from disk into RAM.
    pub reloaded: usize,
}

// ============================================================================
// Write counters (engine 0.5.44)
// ============================================================================

/// What a statement's writes changed (engine 0.5.44 `QueryResult::counters`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WriteCountersInfo {
    /// Nodes created by `INSERT`, `CREATE` or `MERGE`.
    pub nodes_created: u64,
    /// Nodes deleted.
    pub nodes_deleted: u64,
    /// Edges created.
    pub edges_created: u64,
    /// Edges deleted, including those `DETACH DELETE` removes.
    pub edges_deleted: u64,
    /// Property values written or removed, including those of created entities.
    pub properties_set: u64,
    /// Labels added, including those of created nodes.
    pub labels_added: u64,
    /// Labels removed.
    pub labels_removed: u64,
}

impl WriteCountersInfo {
    /// The counters of a query result, or `None` when the statement wrote nothing.
    #[must_use]
    pub fn from_result(result: &grafeo_engine::database::QueryResult) -> Option<Self> {
        let c = &result.counters;
        c.contains_updates().then_some(Self {
            nodes_created: c.nodes_created,
            nodes_deleted: c.nodes_deleted,
            edges_created: c.edges_created,
            edges_deleted: c.edges_deleted,
            properties_set: c.properties_set,
            labels_added: c.labels_added,
            labels_removed: c.labels_removed,
        })
    }

    /// The value of one counter.
    #[must_use]
    pub fn get(&self, counter: WriteCounter) -> u64 {
        match counter {
            WriteCounter::NodesCreated => self.nodes_created,
            WriteCounter::NodesDeleted => self.nodes_deleted,
            WriteCounter::EdgesCreated => self.edges_created,
            WriteCounter::EdgesDeleted => self.edges_deleted,
            WriteCounter::PropertiesSet => self.properties_set,
            WriteCounter::LabelsAdded => self.labels_added,
            WriteCounter::LabelsRemoved => self.labels_removed,
        }
    }

    /// The non-zero counters, in field order.
    #[must_use]
    pub fn non_zero_counters(&self) -> Vec<(WriteCounter, u64)> {
        WriteCounter::ALL
            .into_iter()
            .map(|c| (c, self.get(c)))
            .filter(|&(_, n)| n > 0)
            .collect()
    }

    /// The non-zero counters as `(name, value)` pairs, in field order.
    #[must_use]
    pub fn non_zero(&self) -> Vec<(&'static str, u64)> {
        self.non_zero_counters()
            .into_iter()
            .map(|(c, n)| (c.name(), n))
            .collect()
    }

    /// The non-zero counters under their Bolt (Neo4j `stats`) names, with
    /// values saturated to `i64`.
    #[must_use]
    pub fn non_zero_bolt(&self) -> Vec<(&'static str, i64)> {
        self.non_zero_counters()
            .into_iter()
            .map(|(c, n)| (c.bolt_name(), saturating_i64(n)))
            .collect()
    }
}

/// A counter value as `i64`, saturating at `i64::MAX` (GWP and Bolt carry
/// signed integers).
#[must_use]
pub fn saturating_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// The kinds of write counter. Matches are exhaustive, so a new counter must
/// be named for every transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteCounter {
    /// `nodes_created`.
    NodesCreated,
    /// `nodes_deleted`.
    NodesDeleted,
    /// `edges_created`.
    EdgesCreated,
    /// `edges_deleted`.
    EdgesDeleted,
    /// `properties_set`.
    PropertiesSet,
    /// `labels_added`.
    LabelsAdded,
    /// `labels_removed`.
    LabelsRemoved,
}

impl WriteCounter {
    /// Every counter, in field order.
    pub const ALL: [Self; 7] = [
        Self::NodesCreated,
        Self::NodesDeleted,
        Self::EdgesCreated,
        Self::EdgesDeleted,
        Self::PropertiesSet,
        Self::LabelsAdded,
        Self::LabelsRemoved,
    ];

    /// The snake_case name used by HTTP and GWP.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NodesCreated => "nodes_created",
            Self::NodesDeleted => "nodes_deleted",
            Self::EdgesCreated => "edges_created",
            Self::EdgesDeleted => "edges_deleted",
            Self::PropertiesSet => "properties_set",
            Self::LabelsAdded => "labels_added",
            Self::LabelsRemoved => "labels_removed",
        }
    }

    /// The name Neo4j drivers know in `stats`.
    #[must_use]
    pub const fn bolt_name(self) -> &'static str {
        match self {
            Self::NodesCreated => "nodes-created",
            Self::NodesDeleted => "nodes-deleted",
            Self::EdgesCreated => "relationships-created",
            Self::EdgesDeleted => "relationships-deleted",
            Self::PropertiesSet => "properties-set",
            Self::LabelsAdded => "labels-added",
            Self::LabelsRemoved => "labels-removed",
        }
    }
}

// ============================================================================
// Upsert types (engine 0.5.44)
// ============================================================================

fn default_upsert_key() -> String {
    "id".to_owned()
}

fn default_src_field() -> String {
    "src".to_owned()
}

fn default_dst_field() -> String {
    "dst".to_owned()
}

/// Request for `POST /db/{name}/upsert/nodes`.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpsertNodesRequest {
    /// Labels every node has; a node matches by all of them plus its key.
    pub labels: Vec<String>,
    /// Property that identifies a node. Default: `id`.
    #[serde(default = "default_upsert_key")]
    pub key: String,
    /// One plain JSON object per node. A row without the key is skipped.
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<Object>))]
    pub rows: Vec<serde_json::Value>,
    /// Replace a node's properties with the row's instead of merging. Default: false.
    #[serde(default)]
    pub replace: bool,
    /// Named graph to write to. Default: the default graph.
    #[serde(default)]
    pub graph: Option<String>,
}

/// Request for `POST /db/{name}/upsert/edges`.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpsertEdgesRequest {
    /// Edge type of every edge.
    pub edge_type: String,
    /// Property that identifies an edge between two nodes. Default: `id`.
    #[serde(default = "default_upsert_key")]
    pub key: String,
    /// Node property the source and target fields hold. Default: `id`.
    #[serde(default = "default_upsert_key")]
    pub endpoint_key: String,
    /// Labels an endpoint must have. Default: none.
    #[serde(default)]
    pub endpoint_labels: Vec<String>,
    /// Row field with the source node's key. Default: `src`.
    #[serde(default = "default_src_field")]
    pub src_field: String,
    /// Row field with the target node's key. Default: `dst`.
    #[serde(default = "default_dst_field")]
    pub dst_field: String,
    /// One plain JSON object per edge. Every other field is an edge property.
    /// A row is skipped when it lacks the key or an endpoint field, or when
    /// no node or more than one node has its endpoint key.
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<Object>))]
    pub rows: Vec<serde_json::Value>,
    /// Replace an edge's properties with the row's instead of merging. Default: false.
    #[serde(default)]
    pub replace: bool,
    /// Named graph to write to. Default: the default graph.
    #[serde(default)]
    pub graph: Option<String>,
}

/// What an upsert did with its rows.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpsertResponse {
    /// Rows that created a node or edge.
    pub created: usize,
    /// Rows that updated an existing node or edge.
    pub updated: usize,
    /// Rows that were not written.
    pub skipped: usize,
    /// Indices of the skipped rows, in order (at most 1,000).
    pub skipped_rows: Vec<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // TokenScopeRequest::to_role
    // -----------------------------------------------------------------------

    #[test]
    fn to_role_admin() {
        let req = TokenScopeRequest {
            role: "admin".to_string(),
            databases: vec![],
        };
        assert_eq!(req.to_role().unwrap(), grafeo_engine::auth::Role::Admin);
    }

    #[test]
    fn to_role_read_write() {
        let req = TokenScopeRequest {
            role: "read-write".to_string(),
            databases: vec![],
        };
        assert_eq!(req.to_role().unwrap(), grafeo_engine::auth::Role::ReadWrite);
    }

    #[test]
    fn to_role_read_only() {
        let req = TokenScopeRequest {
            role: "read-only".to_string(),
            databases: vec![],
        };
        assert_eq!(req.to_role().unwrap(), grafeo_engine::auth::Role::ReadOnly);
    }

    #[test]
    fn to_role_invalid() {
        let req = TokenScopeRequest {
            role: "superuser".to_string(),
            databases: vec![],
        };
        assert!(req.to_role().is_err());
    }

    #[test]
    fn to_role_default() {
        let req = TokenScopeRequest::default();
        assert_eq!(req.to_role().unwrap(), grafeo_engine::auth::Role::ReadOnly);
    }

    #[test]
    fn to_role_empty_string_is_error() {
        let req = TokenScopeRequest {
            role: String::new(),
            databases: vec![],
        };
        let err = req.to_role().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown role"),
            "error message should mention unknown role, got: {msg}"
        );
    }

    #[test]
    fn to_role_case_sensitive() {
        let req = TokenScopeRequest {
            role: "Admin".to_string(),
            databases: vec![],
        };
        assert!(
            req.to_role().is_err(),
            "role matching should be case-sensitive"
        );
    }

    #[test]
    fn token_scope_request_default_fields() {
        let req = TokenScopeRequest::default();
        assert_eq!(req.role, "read-only");
        assert_eq!(req.databases, [] as [std::string::String; 0]);
    }

    #[test]
    fn token_scope_request_serde_default_role() {
        // Deserialize with no role field: should use the serde default ("read-only")
        let json = r#"{"databases": ["mydb"]}"#;
        let req: TokenScopeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.role, "read-only");
        assert_eq!(req.databases, vec!["mydb"]);
    }

    #[test]
    fn token_scope_request_serde_roundtrip() {
        let req = TokenScopeRequest {
            role: "read-only".to_string(),
            databases: vec!["db1".to_string(), "db2".to_string()],
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: TokenScopeRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.role, "read-only");
        assert_eq!(back.databases, vec!["db1", "db2"]);
    }

    // -----------------------------------------------------------------------
    // DatabaseType
    // -----------------------------------------------------------------------

    #[test]
    fn database_type_display() {
        assert_eq!(DatabaseType::Lpg.to_string(), "lpg");
        assert_eq!(DatabaseType::Rdf.to_string(), "rdf");
        assert_eq!(DatabaseType::OwlSchema.to_string(), "owl-schema");
        assert_eq!(DatabaseType::RdfsSchema.to_string(), "rdfs-schema");
        assert_eq!(DatabaseType::JsonSchema.to_string(), "json-schema");
    }

    #[test]
    fn database_type_default_is_lpg() {
        assert_eq!(DatabaseType::default(), DatabaseType::Lpg);
    }

    #[test]
    fn storage_mode_default_is_in_memory() {
        assert_eq!(StorageMode::default(), StorageMode::InMemory);
    }

    #[test]
    fn storage_mode_as_str() {
        assert_eq!(StorageMode::InMemory.as_str(), "in-memory");
        assert_eq!(StorageMode::Persistent.as_str(), "persistent");
    }

    // -----------------------------------------------------------------------
    // Storage tier types
    // -----------------------------------------------------------------------

    #[test]
    fn storage_tiers_response_serializes_section_keys_as_strings() {
        let resp = StorageTiersResponse {
            tiers: vec![
                SectionTierInfo {
                    section: "VectorStore".to_string(),
                    tier: "in_memory".to_string(),
                },
                SectionTierInfo {
                    section: "CompactStore".to_string(),
                    tier: "on_disk".to_string(),
                },
            ],
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["tiers"][0]["section"], "VectorStore");
        assert_eq!(json["tiers"][0]["tier"], "in_memory");
        assert_eq!(json["tiers"][1]["tier"], "on_disk");
    }

    #[test]
    fn reload_eligible_request_target_fraction_is_optional() {
        // The 0.7 default is applied by AdminService::reload_eligible, not by
        // deserialization; an empty body leaves the field unset.
        let req: ReloadEligibleRequest = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(req.target_fraction, None);
    }

    #[test]
    fn write_counter_saturates_and_names_are_distinct() {
        assert_eq!(saturating_i64(u64::MAX), i64::MAX);
        assert_eq!(saturating_i64(5), 5);
        let info = WriteCountersInfo {
            nodes_created: u64::MAX,
            edges_deleted: 3,
            ..Default::default()
        };
        assert_eq!(
            info.non_zero_bolt(),
            vec![("nodes-created", i64::MAX), ("relationships-deleted", 3)]
        );
        let mut names: Vec<_> = WriteCounter::ALL.iter().map(|c| c.bolt_name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 7);
        assert!(WriteCounter::ALL.iter().all(|c| !c.name().contains('-')));
    }

    #[test]
    fn write_counters_info_is_none_for_reads() {
        let mut result = grafeo_engine::database::QueryResult::empty();
        assert_eq!(WriteCountersInfo::from_result(&result), None);
        result.counters.nodes_created = 2;
        result.counters.labels_added = 2;
        let info = WriteCountersInfo::from_result(&result).unwrap();
        assert_eq!(info.nodes_created, 2);
        assert_eq!(
            info.non_zero(),
            vec![("nodes_created", 2), ("labels_added", 2)]
        );
    }
}
