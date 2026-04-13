import { useCallback, useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { api } from "../../api/client";
import type { DatabaseSummary, DatabaseStatsResponse } from "../../types/api";
import styles from "./DatabaseCards.module.css";

interface DbCardData extends DatabaseSummary {
  stats?: DatabaseStatsResponse;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024)
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

/**
 * Clickable overview cards for the Databases list page. Cards are
 * observation-only: all ops (backup, restore, delete) live on the
 * per-db details page. Click a card → navigate to /databases/{name}.
 */
export default function DatabaseCards() {
  const [databases, setDatabases] = useState<DbCardData[]>([]);
  const [loading, setLoading] = useState(true);

  const refresh = useCallback(() => {
    api.db
      .list()
      .then(async (r) => {
        const withStats = await Promise.all(
          r.databases.map(async (db) => {
            try {
              const stats = await api.admin.stats(db.name);
              return { ...db, stats };
            } catch {
              return { ...db };
            }
          }),
        );
        setDatabases(withStats);
      })
      .catch(() => {})
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  if (loading) {
    return <div className={styles.empty}>Loading…</div>;
  }

  if (databases.length === 0) {
    return <div className={styles.empty}>No databases yet.</div>;
  }

  return (
    <div className={styles.grid}>
      {databases.map((db) => {
        const nodes = db.stats?.node_count ?? db.node_count;
        const edges = db.stats?.edge_count ?? db.edge_count;
        const memory = db.stats?.memory_bytes;
        const showTypeBadge = db.database_type.toLowerCase() !== "lpg";
        return (
          <Link
            key={db.name}
            to={`/databases/${encodeURIComponent(db.name)}`}
            className={styles.card}
          >
            <div className={styles.cardHeader}>
              <span className={styles.name}>{db.name}</span>
              {showTypeBadge && (
                <span className={styles.typeBadge}>
                  {db.database_type.toUpperCase()}
                </span>
              )}
            </div>
            <div className={styles.stats}>
              <span className={styles.counts}>
                {nodes.toLocaleString()}n · {edges.toLocaleString()}e
              </span>
              <span className={styles.meta}>
                {db.persistent ? "Persistent" : "In-memory"}
                {memory != null && ` · ${formatBytes(memory)}`}
              </span>
            </div>
          </Link>
        );
      })}
    </div>
  );
}
