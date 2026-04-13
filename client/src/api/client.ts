import type {
  QueryRequest,
  QueryResponse,
  TransactionResponse,
  HealthResponse,
  ListDatabasesResponse,
  DatabaseSummary,
  DatabaseInfoResponse,
  DatabaseStatsResponse,
  DatabaseSchemaResponse,
  CreateDatabaseRequest,
  SystemResources,
  WalStatusInfo,
  ValidationInfo,
  BackupEntry,
  TokenResponse,
  CreateTokenRequest,
} from "../types/api";

export class GrafeoApiError extends Error {
  constructor(
    public status: number,
    public detail: string,
  ) {
    super(detail);
    this.name = "GrafeoApiError";
  }
}

let authToken: string | null = null;

/** Set a Bearer token for all subsequent API requests. Pass null to clear. */
export function setAuthToken(token: string | null): void {
  authToken = token;
}

async function request<T>(path: string, options: RequestInit = {}): Promise<T> {
  const headers: Record<string, string> = {
    "Content-Type": "application/json",
    ...options.headers as Record<string, string>,
  };
  if (authToken) {
    headers["Authorization"] = `Bearer ${authToken}`;
  }
  const res = await fetch(path, { ...options, headers });
  if (!res.ok) {
    const body = await res.json().catch(() => ({ detail: res.statusText }));
    throw new GrafeoApiError(res.status, body.detail ?? res.statusText);
  }
  // 204 No Content and any other empty body: don't try to parse JSON.
  // Endpoints like POST /admin/{db}/restore/epoch return 204 on success;
  // calling res.json() on an empty body throws SyntaxError and the call
  // site treats the successful restore as a failure.
  if (res.status === 204 || res.headers.get("content-length") === "0") {
    return undefined as T;
  }
  return res.json();
}

export const api = {
  health: () => request<HealthResponse>("/health"),

  query: (body: QueryRequest) =>
    request<QueryResponse>("/query", {
      method: "POST",
      body: JSON.stringify(body),
    }),

  cypher: (body: QueryRequest) =>
    request<QueryResponse>("/cypher", {
      method: "POST",
      body: JSON.stringify(body),
    }),

  graphql: (body: QueryRequest) =>
    request<QueryResponse>("/graphql", {
      method: "POST",
      body: JSON.stringify(body),
    }),

  gremlin: (body: QueryRequest) =>
    request<QueryResponse>("/gremlin", {
      method: "POST",
      body: JSON.stringify(body),
    }),

  sparql: (body: QueryRequest) =>
    request<QueryResponse>("/sparql", {
      method: "POST",
      body: JSON.stringify(body),
    }),

  tx: {
    begin: (database?: string) =>
      request<TransactionResponse>("/tx/begin", {
        method: "POST",
        body: database ? JSON.stringify({ database }) : undefined,
      }),

    query: (sessionId: string, body: QueryRequest) =>
      request<QueryResponse>("/tx/query", {
        method: "POST",
        headers: { "X-Session-Id": sessionId },
        body: JSON.stringify(body),
      }),

    commit: (sessionId: string) =>
      request<TransactionResponse>("/tx/commit", {
        method: "POST",
        headers: { "X-Session-Id": sessionId },
      }),

    rollback: (sessionId: string) =>
      request<TransactionResponse>("/tx/rollback", {
        method: "POST",
        headers: { "X-Session-Id": sessionId },
      }),
  },

  db: {
    list: () => request<ListDatabasesResponse>("/db"),

    create: (req: CreateDatabaseRequest) =>
      request<DatabaseSummary>("/db", {
        method: "POST",
        body: JSON.stringify(req),
      }),

    delete: (name: string) =>
      request<{ deleted: string }>(`/db/${encodeURIComponent(name)}`, {
        method: "DELETE",
      }),

    info: (name: string) =>
      request<DatabaseInfoResponse>(`/db/${encodeURIComponent(name)}`),

    stats: (name: string) =>
      request<DatabaseStatsResponse>(`/db/${encodeURIComponent(name)}/stats`),

    schema: (name: string) =>
      request<DatabaseSchemaResponse>(`/db/${encodeURIComponent(name)}/schema`),
  },

  system: {
    resources: () => request<SystemResources>("/system/resources"),
  },

  admin: {
    stats: (db: string) =>
      request<DatabaseStatsResponse>(`/admin/${encodeURIComponent(db)}/stats`),

    walStatus: (db: string) =>
      request<WalStatusInfo>(`/admin/${encodeURIComponent(db)}/wal`),

    memory: (db: string) =>
      request<Record<string, unknown>>(`/admin/${encodeURIComponent(db)}/memory`),

    validate: (db: string) =>
      request<ValidationInfo>(`/admin/${encodeURIComponent(db)}/validate`),

    checkpoint: (db: string) =>
      request<{ success: boolean }>(`/admin/${encodeURIComponent(db)}/wal/checkpoint`, {
        method: "POST",
      }),
  },

  backup: {
    create: (db: string, label?: string) =>
      request<BackupEntry>(`/admin/${encodeURIComponent(db)}/backup`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(label ? { label } : {}),
      }),

    // Kept wired for future re-enablement; currently not surfaced in the UI.
    // Engine 0.5.37's do_backup_incremental() always returns "no new WAL
    // records since last backup" even when the WAL has clearly advanced
    // (grafeo-engine issue filed separately). Flip the Incremental toggle
    // back on in CreateBackupDialog once that lands.
    createIncremental: (db: string) =>
      request<BackupEntry>(
        `/admin/${encodeURIComponent(db)}/backup/incremental`,
        { method: "POST" },
      ),

    list: (db: string) =>
      request<BackupEntry[]>(`/admin/${encodeURIComponent(db)}/backups`),

    listAll: () => request<BackupEntry[]>("/backups"),

    restore: (targetDb: string, backup: string, sourceDb?: string) =>
      request<{ restored: boolean }>(
        `/admin/${encodeURIComponent(targetDb)}/restore`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(
            sourceDb ? { backup, source_db: sourceDb } : { backup },
          ),
        },
      ),

    restoreToEpoch: (db: string, epoch: number) =>
      request<null>(`/admin/${encodeURIComponent(db)}/restore/epoch`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ epoch }),
      }),

    remove: (db: string, filename: string) =>
      request<{ deleted: boolean }>(
        `/admin/${encodeURIComponent(db)}/backups/${encodeURIComponent(filename)}`,
        { method: "DELETE" },
      ),

    downloadUrl: (db: string, filename: string) =>
      `/admin/${encodeURIComponent(db)}/backups/download/${encodeURIComponent(filename)}`,
  },

  tokens: {
    list: () => request<TokenResponse[]>("/admin/tokens"),

    create: (req: CreateTokenRequest) =>
      request<TokenResponse>("/admin/tokens", {
        method: "POST",
        body: JSON.stringify(req),
      }),

    get: (id: string) =>
      request<TokenResponse>(`/admin/tokens/${encodeURIComponent(id)}`),

    delete: (id: string) =>
      request<{ deleted: boolean }>(`/admin/tokens/${encodeURIComponent(id)}`, {
        method: "DELETE",
      }),
  },
};
