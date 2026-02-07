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

export interface DatabaseSummary {
  name: string;
  node_count: number;
  edge_count: number;
  persistent: boolean;
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
