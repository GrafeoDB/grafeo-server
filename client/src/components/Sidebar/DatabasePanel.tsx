import { useState, useEffect, useCallback } from "react";
import { api, GrafeoApiError } from "../../api/client";
import type { DatabaseSummary } from "../../types/api";
import styles from "./DatabasePanel.module.css";

interface DatabasePanelProps {
  currentDatabase: string;
  onSelectDatabase: (name: string) => void;
}

export default function DatabasePanel({
  currentDatabase,
  onSelectDatabase,
}: DatabasePanelProps) {
  const [databases, setDatabases] = useState<DatabaseSummary[]>([]);
  const [newName, setNewName] = useState("");
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    api.db.list().then((res) => setDatabases(res.databases)).catch(() => {});
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const handleCreate = async () => {
    const name = newName.trim();
    if (!name) return;
    setError(null);
    try {
      await api.db.create(name);
      setNewName("");
      refresh();
    } catch (err) {
      if (err instanceof GrafeoApiError) {
        setError(err.detail);
      } else {
        setError(String(err));
      }
    }
  };

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
              <span className={styles.dbCounts}>
                {db.node_count}n/{db.edge_count}e
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

      <div className={styles.createRow}>
        <input
          className={styles.createInput}
          placeholder="New database..."
          value={newName}
          onChange={(e) => setNewName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") handleCreate();
          }}
        />
        <button
          className={styles.createButton}
          onClick={handleCreate}
          disabled={!newName.trim()}
        >
          Create
        </button>
      </div>

      {error && <div className={styles.error}>{error}</div>}
    </>
  );
}
