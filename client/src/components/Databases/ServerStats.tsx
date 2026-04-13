import { useEffect, useState } from "react";
import { api } from "../../api/client";
import type { HealthResponse, DatabaseSummary } from "../../types/api";
import styles from "./ServerStats.module.css";

function formatUptime(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  return `${h}h ${m}m`;
}

interface Tile {
  label: string;
  value: string | number;
}

/**
 * Compact strip of server-level stats shown at the top of the Databases
 * list page. Pulls health and db list and reduces to totals.
 */
export default function ServerStats() {
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [databases, setDatabases] = useState<DatabaseSummary[]>([]);

  useEffect(() => {
    api.health().then(setHealth).catch(() => {});
    api.db.list().then((r) => setDatabases(r.databases)).catch(() => {});
  }, []);

  if (!health) {
    return <div className={styles.loading}>Loading server stats…</div>;
  }

  const totalNodes = databases.reduce((n, d) => n + d.node_count, 0);
  const totalEdges = databases.reduce((n, d) => n + d.edge_count, 0);

  const tiles: Tile[] = [
    { label: "Version", value: `v${health.version}` },
    { label: "Engine", value: `v${health.engine_version}` },
    { label: "Storage", value: health.persistent ? "Persistent" : "In-memory" },
    {
      label: "Uptime",
      value: health.uptime_seconds != null ? formatUptime(health.uptime_seconds) : "—",
    },
    { label: "Sessions", value: health.active_sessions ?? 0 },
    { label: "Databases", value: databases.length },
    { label: "Nodes", value: totalNodes.toLocaleString() },
    { label: "Edges", value: totalEdges.toLocaleString() },
  ];

  return (
    <div className={styles.grid}>
      {tiles.map((t) => (
        <div key={t.label} className={styles.tile}>
          <span className={styles.tileLabel}>{t.label}</span>
          <span className={styles.tileValue}>{t.value}</span>
        </div>
      ))}
    </div>
  );
}
