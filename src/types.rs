//! Transport-agnostic types used across both HTTP and GWP layers.
//!
//! These types are shared between `database_manager`, `schema`, and the
//! transport-specific route/handler modules. They live here to avoid
//! coupling the database layer to any particular transport (HTTP/gRPC).

use serde::{Deserialize, Serialize};

/// Request to create a new named database.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "http", derive(utoipa::ToSchema))]
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
#[cfg_attr(feature = "http", derive(utoipa::ToSchema))]
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
#[cfg_attr(feature = "http", derive(utoipa::ToSchema))]
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
#[cfg_attr(feature = "http", derive(utoipa::ToSchema))]
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
