import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { api } from "../api/client";
import type { RefusedKind } from "../api/client";
import { settle } from "../hooks/useLoaded";
import type { DatabaseSchemaResponse, DatabaseSummary, HealthResponse } from "../types/api";
import { readStored, writeStored } from "../storage";

const DB_KEY = "grafeo-current-database";
const HEALTH_POLL_MS = 15_000;

/** A request's state: not answered yet, answered, refused (401/403), or failed otherwise. */
export type Loaded<T> =
  | { state: "loading" }
  | { state: "ready"; data: T }
  | { state: "refused"; refused: RefusedKind }
  | { state: "failed"; message: string };

export interface ServerContextValue {
  /** The server's health, or null while it has not answered (or cannot be reached). */
  health: HealthResponse | null;
  reachable: boolean | null;
  databases: Loaded<DatabaseSummary[]>;
  refreshDatabases(): void;
  /** The database queries run on. */
  activeDatabase: string;
  selectDatabase(name: string): void;
  /** The active database's graph model, lower case as the server names it (`lpg`, `rdf`). */
  activeModel: string;
  schema: Loaded<DatabaseSchemaResponse>;
  refreshSchema(): void;
  /** The server checks credentials, so token management applies. */
  authEnabled: boolean;
}

const ServerContext = createContext<ServerContextValue | null>(null);

export function ServerProvider({ children }: { children: ReactNode }) {
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [reachable, setReachable] = useState<boolean | null>(null);
  const [databases, setDatabases] = useState<Loaded<DatabaseSummary[]>>({ state: "loading" });
  const [storedDatabase, setStoredDatabase] = useState(() => readStored(DB_KEY) || "default");
  const [schemaAnswer, setSchemaAnswer] = useState<{ key: string; value: Loaded<DatabaseSchemaResponse> } | null>(null);
  const [schemaTick, setSchemaTick] = useState(0);
  const [databasesTick, setDatabasesTick] = useState(0);

  useEffect(() => {
    let cancelled = false;
    const probe = () =>
      api
        .health()
        .then((h) => {
          if (cancelled) return;
          setHealth(h);
          setReachable(true);
        })
        .catch(() => {
          if (cancelled) return;
          setHealth(null);
          setReachable(false);
        });
    probe();
    const timer = setInterval(probe, HEALTH_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    settle(
      api.db.list().then((res) => res.databases),
      (value) => !cancelled && setDatabases(value),
    );
    return () => {
      cancelled = true;
    };
  }, [databasesTick]);

  // A stored database that no longer exists gives way to the first one listed.
  const activeDatabase =
    databases.state === "ready" &&
    databases.data.length > 0 &&
    !databases.data.some((d) => d.name === storedDatabase)
      ? databases.data[0].name
      : storedDatabase;

  useEffect(() => {
    let cancelled = false;
    const key = `${activeDatabase}
${schemaTick}`;
    settle(api.db.schema(activeDatabase), (value) => !cancelled && setSchemaAnswer({ key, value }));
    return () => {
      cancelled = true;
    };
  }, [activeDatabase, schemaTick]);
  const schema = useMemo<Loaded<DatabaseSchemaResponse>>(
    () =>
      schemaAnswer && schemaAnswer.key.startsWith(`${activeDatabase}
`) ? schemaAnswer.value : { state: "loading" },
    [schemaAnswer, activeDatabase],
  );

  const selectDatabase = useCallback((name: string) => {
    setStoredDatabase(name);
    writeStored(DB_KEY, name);
  }, []);

  const refreshDatabases = useCallback(() => setDatabasesTick((t) => t + 1), []);
  const refreshSchema = useCallback(() => setSchemaTick((t) => t + 1), []);

  const activeModel = useMemo(() => {
    if (databases.state !== "ready") return "lpg";
    const type = databases.data.find((d) => d.name === activeDatabase)?.database_type ?? "lpg";
    return type.toLowerCase();
  }, [databases, activeDatabase]);

  const authEnabled = health?.features?.server?.includes("auth") ?? false;

  const value = useMemo<ServerContextValue>(
    () => ({
      health,
      reachable,
      databases,
      refreshDatabases,
      activeDatabase,
      selectDatabase,
      activeModel,
      schema,
      refreshSchema,
      authEnabled,
    }),
    [health, reachable, databases, refreshDatabases, activeDatabase, selectDatabase, activeModel, schema, refreshSchema, authEnabled],
  );
  return <ServerContext.Provider value={value}>{children}</ServerContext.Provider>;
}

export function useServer(): ServerContextValue {
  const ctx = useContext(ServerContext);
  if (!ctx) throw new Error("useServer must be used within ServerProvider");
  return ctx;
}

/** RDF databases run SPARQL (and GQL); every other model runs the property-graph languages. */
export function isRdfModel(model: string): boolean {
  return model === "rdf" || model === "owl-schema" || model === "rdfs-schema";
}
