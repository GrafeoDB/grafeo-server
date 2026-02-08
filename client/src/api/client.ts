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
};
