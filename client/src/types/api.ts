export interface QueryRequest {
  query: string;
  params?: Record<string, unknown>;
  language?: "gql" | "cypher" | "gremlin" | "graphql" | "sparql";
  database?: string;
  timeout_ms?: number;
}

export interface QueryResponse {
  columns: string[];
  rows: unknown[][];
  execution_time_ms?: number;
  rows_scanned?: number;
}

export interface TransactionResponse {
  session_id: string;
  status: "open" | "committed" | "rolled_back";
}

export interface HealthResponse {
  status: string;
  version: string;
  engine_version: string;
  persistent: boolean;
  uptime_seconds: number;
  active_sessions: number;
}

export type DatabaseType =
  | "Lpg"
  | "Rdf"
  | "OwlSchema"
  | "RdfsSchema"
  | "JsonSchema";

export type StorageMode = "InMemory" | "Persistent";

export interface DatabaseOptions {
  memory_limit_bytes?: number;
  wal_enabled?: boolean;
  wal_durability?: "sync" | "batch" | "adaptive" | "nosync";
  backward_edges?: boolean;
  threads?: number;
  spill_path?: string;
}

export interface CreateDatabaseRequest {
  name: string;
  database_type?: DatabaseType;
  storage_mode?: StorageMode;
  options?: DatabaseOptions;
  schema_file?: string;
  schema_filename?: string;
}

export interface DatabaseSummary {
  name: string;
  node_count: number;
  edge_count: number;
  persistent: boolean;
  database_type: string;
}

export interface ListDatabasesResponse {
  databases: DatabaseSummary[];
}

export interface DatabaseInfoResponse {
  name: string;
  node_count: number;
  edge_count: number;
  persistent: boolean;
  version: string;
  wal_enabled: boolean;
  database_type: string;
  storage_mode: string;
  memory_limit_bytes?: number;
  backward_edges: boolean;
  threads: number;
}

export interface DatabaseStatsResponse {
  name: string;
  node_count: number;
  edge_count: number;
  label_count: number;
  edge_type_count: number;
  property_key_count: number;
  index_count: number;
  memory_bytes: number;
  disk_bytes?: number;
}

export interface DatabaseSchemaResponse {
  name: string;
  labels: { name: string; count: number }[];
  edge_types: { name: string; count: number }[];
  property_keys: string[];
}

export interface ResourceDefaults {
  memory_limit_bytes: number;
  storage_mode: string;
  wal_enabled: boolean;
  wal_durability: string;
  backward_edges: boolean;
  threads: number;
}

export interface SystemResources {
  total_memory_bytes: number;
  allocated_memory_bytes: number;
  available_memory_bytes: number;
  available_disk_bytes: number | null;
  persistent_available: boolean;
  available_types: string[];
  defaults: ResourceDefaults;
}
