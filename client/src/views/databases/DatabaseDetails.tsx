import { useCallback, useEffect, useState } from "react";
import { Link, useParams } from "react-router-dom";
import { api, GrafeoApiError } from "../../api/client";
import type {
  DatabaseSummary,
  DatabaseStatsResponse,
  WalStatusInfo,
} from "../../types/api";
import BackupsSection from "../../components/Databases/BackupsSection";
import DangerZone from "../../components/Databases/DangerZone";
import styles from "./DatabaseDetails.module.css";

function formatBytes(bytes: number | undefined): string {
  if (bytes == null) return "—";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024)
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

export default function DatabaseDetails() {
  const { name = "" } = useParams<{ name: string }>();
  const [summary, setSummary] = useState<DatabaseSummary | null>(null);
  const [stats, setStats] = useState<DatabaseStatsResponse | null>(null);
  const [wal, setWal] = useState<WalStatusInfo | null>(null);
  const [notFound, setNotFound] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [refreshKey, setRefreshKey] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setNotFound(false);
    setLoadError(null);
    setSummary(null);
    setStats(null);
    setWal(null);

    // Only the db list decides not-found. Stats + WAL are best-effort
    // details that swallow their own errors so a missing stats endpoint
    // doesn't break the page.
    api.db
      .list()
      .then(async (dbs) => {
        if (cancelled) return;
        const found = dbs.databases.find((d) => d.name === name) ?? null;
        if (!found) {
          setNotFound(true);
          return;
        }
        setSummary(found);
        const [s, w] = await Promise.all([
          api.admin.stats(name).catch(() => null),
          api.admin.walStatus(name).catch(() => null),
        ]);
        if (!cancelled) {
          setStats(s);
          setWal(w);
        }
      })
      .catch((err) => {
        if (cancelled) return;
        // Actual fetch failure (network / auth / 5xx) — don't pretend
        // the database was deleted. Surface the real reason so the user
        // can retry or check credentials.
        const detail =
          err instanceof GrafeoApiError ? err.detail : String(err);
        setLoadError(detail);
      });

    return () => {
      cancelled = true;
    };
  }, [name, refreshKey]);

  const refresh = useCallback(() => setRefreshKey((k) => k + 1), []);

  if (notFound) {
    return (
      <div className={styles.page}>
        <div className={styles.missing}>
          Database <code>{name}</code> not found.{" "}
          <Link to="/databases" className={styles.backLink}>
            ← Back to databases
          </Link>
        </div>
      </div>
    );
  }

  if (loadError) {
    return (
      <div className={styles.page}>
        <div className={styles.missing}>
          Couldn't load <code>{name}</code>: {loadError}.{" "}
          <button
            type="button"
            className={styles.backLink}
            onClick={refresh}
          >
            Retry
          </button>
        </div>
      </div>
    );
  }

  const nodes = stats?.node_count ?? summary?.node_count ?? 0;
  const edges = stats?.edge_count ?? summary?.edge_count ?? 0;
  const memory = stats?.memory_bytes;
  const disk = stats?.disk_bytes;
  const typeBadge =
    summary?.database_type && summary.database_type.toLowerCase() !== "lpg"
      ? summary.database_type.toUpperCase()
      : null;

  return (
    <div className={styles.page}>
      <div className={styles.breadcrumb}>
        <Link to="/databases" className={styles.breadcrumbLink}>
          Databases
        </Link>
        <span className={styles.breadcrumbSep}>/</span>
        <span className={styles.breadcrumbCurrent}>{name}</span>
      </div>

      <header className={styles.header}>
        <div className={styles.titleRow}>
          <h1 className={styles.title}>{name}</h1>
          {typeBadge && <span className={styles.typeBadge}>{typeBadge}</span>}
        </div>
        {summary && (
          <div className={styles.statsLine}>
            <span>{nodes.toLocaleString()} nodes</span>
            <span className={styles.statsSep}>·</span>
            <span>{edges.toLocaleString()} edges</span>
            <span className={styles.statsSep}>·</span>
            <span>{summary.persistent ? "Persistent" : "In-memory"}</span>
            {memory != null && (
              <>
                <span className={styles.statsSep}>·</span>
                <span>{formatBytes(memory)} memory</span>
              </>
            )}
            {disk != null && (
              <>
                <span className={styles.statsSep}>·</span>
                <span>{formatBytes(disk)} disk</span>
              </>
            )}
          </div>
        )}
        {stats && (
          <div className={styles.statsLine}>
            <span>{stats.label_count.toLocaleString()} labels</span>
            <span className={styles.statsSep}>·</span>
            <span>{stats.edge_type_count.toLocaleString()} edge types</span>
            <span className={styles.statsSep}>·</span>
            <span>
              {stats.property_key_count.toLocaleString()} property keys
            </span>
            <span className={styles.statsSep}>·</span>
            <span>{stats.index_count.toLocaleString()} indexes</span>
          </div>
        )}
        {wal && (
          <div className={styles.statsLine}>
            <span className={styles.walLabel}>WAL</span>
            {wal.enabled ? (
              <>
                <span className={styles.statsSep}>·</span>
                <span>{formatBytes(wal.size_bytes)}</span>
                <span className={styles.statsSep}>·</span>
                <span>
                  {wal.record_count.toLocaleString()} pending record
                  {wal.record_count === 1 ? "" : "s"}
                </span>
                <span className={styles.statsSep}>·</span>
                <span>epoch {wal.current_epoch.toLocaleString()}</span>
                {wal.last_checkpoint != null && (
                  <>
                    <span className={styles.statsSep}>·</span>
                    <span>
                      last checkpoint{" "}
                      {new Date(wal.last_checkpoint * 1000).toLocaleString()}
                    </span>
                  </>
                )}
              </>
            ) : (
              <>
                <span className={styles.statsSep}>·</span>
                <span>disabled</span>
              </>
            )}
          </div>
        )}
      </header>

      <BackupsSection database={name} onMutated={refresh} />

      <DangerZone database={name} />
    </div>
  );
}
