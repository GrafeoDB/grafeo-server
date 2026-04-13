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

export interface EnabledFeatures {
  languages: string[];
  engine: string[];
  server: string[];
}

export interface HealthResponse {
  status: string;
  version: string;
  engine_version: string;
  persistent: boolean;
  read_only: boolean;
  uptime_seconds: number;
  active_sessions: number;
  features: EnabledFeatures;
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

// Admin types

export interface WalStatusInfo {
  enabled: boolean;
  path?: string;
  size_bytes: number;
  record_count: number;
  last_checkpoint?: number;
  current_epoch: number;
}

export interface ValidationInfo {
  valid: boolean;
  errors: { code: string; message: string; context?: string }[];
  warnings: { code: string; message: string; context?: string }[];
}

export interface BackupEntry {
  filename: string;
  database: string;
  kind: string;
  size_bytes: number;
  created_at: string;
  start_epoch: number;
  end_epoch: number;
  checksum: number;
  /** Optional user-supplied label. Stored in a server-side sidecar
   *  and merged into the entry on listing. */
  label?: string;
}

export interface CreateBackupRequest {
  /** Optional label: ^[A-Za-z0-9_-]{1,32}$ when set. Empty string or
   *  undefined means unlabeled. */
  label?: string;
}

// Token management types

export interface TokenScope {
  role: string;
  databases: string[];
}

export interface TokenResponse {
  id: string;
  name: string;
  scope: TokenScope;
  created_at: string;
  token?: string;
}

export interface CreateTokenRequest {
  name: string;
  scope?: TokenScope;
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
