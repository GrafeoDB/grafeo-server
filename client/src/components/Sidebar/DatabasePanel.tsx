import { useState, useEffect, useCallback } from "react";
import { api, GrafeoApiError } from "../../api/client";
import type { DatabaseSummary } from "../../types/api";
import CreateDatabaseDialog from "./CreateDatabaseDialog";
import styles from "./DatabasePanel.module.css";

const TYPE_BADGES: Record<string, string> = {
  lpg: "LPG",
  rdf: "RDF",
  "owl-schema": "OWL",
  "rdfs-schema": "RDFS",
  "json-schema": "JSON",
};

interface DatabasePanelProps {
  currentDatabase: string;
  onSelectDatabase: (name: string) => void;
}

export default function DatabasePanel({
  currentDatabase,
  onSelectDatabase,
}: DatabasePanelProps) {
  const [databases, setDatabases] = useState<DatabaseSummary[]>([]);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    api.db.list().then((res) => setDatabases(res.databases)).catch(() => {});
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const handleDelete = async (name: string) => {
    if (!window.confirm(`Delete database "${name}"? This cannot be undone.`)) {
      return;
    }
    setError(null);
    try {
      await api.db.delete(name);
      if (currentDatabase === name) {
        onSelectDatabase("default");
      }
      refresh();
    } catch (err) {
      if (err instanceof GrafeoApiError) {
        setError(err.detail);
      } else {
        setError(String(err));
      }
    }
  };

  return (
    <>
      <ul className={styles.dbList}>
        {databases.map((db) => (
          <li key={db.name} className={styles.dbItem}>
            <button
              className={`${styles.dbButton} ${db.name === currentDatabase ? styles.active : ""}`}
              onClick={() => onSelectDatabase(db.name)}
              title={`${db.node_count} nodes, ${db.edge_count} edges`}
            >
              <span className={styles.dbName}>{db.name}</span>
              <span className={styles.dbMeta}>
                {db.database_type && db.database_type !== "lpg" && (
                  <span className={styles.typeBadge}>
                    {TYPE_BADGES[db.database_type] ?? db.database_type}
                  </span>
                )}
                <span className={styles.dbCounts}>
                  {db.node_count}n/{db.edge_count}e
                </span>
              </span>
            </button>
            <button
              className={styles.deleteButton}
              onClick={() => handleDelete(db.name)}
              disabled={db.name === "default"}
              title={db.name === "default" ? "Cannot delete default" : `Delete ${db.name}`}
            >
              x
            </button>
          </li>
        ))}
      </ul>

      <button
        className={styles.newDbButton}
        onClick={() => setDialogOpen(true)}
      >
        + New Database
      </button>

      {error && <div className={styles.error}>{error}</div>}

      <CreateDatabaseDialog
        open={dialogOpen}
        onClose={() => setDialogOpen(false)}
        onCreated={refresh}
      />
    </>
  );
}
