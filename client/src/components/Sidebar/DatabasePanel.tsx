import { useState, useEffect, useCallback } from "react";
import { Link } from "react-router-dom";
import { api } from "../../api/client";
import type { DatabaseSummary } from "../../types/api";
import CreateDatabaseDialog from "./CreateDatabaseDialog";
import styles from "./DatabasePanel.module.css";

interface DatabasePanelProps {
  currentDatabase: string;
  onSelectDatabase: (name: string, dbType?: string) => void;
}

/**
 * Narrow-width database picker for the Studio sidebar. A `<select>` is
 * used (instead of a scrollable list) so the control stays a constant
 * height regardless of how many databases the server has. The "Manage"
 * link opens the currently-selected db's details page, bridging the
 * query-context and db-management surfaces.
 */
export default function DatabasePanel({
  currentDatabase,
  onSelectDatabase,
}: DatabasePanelProps) {
  const [databases, setDatabases] = useState<DatabaseSummary[]>([]);
  const [dialogOpen, setDialogOpen] = useState(false);

  const refresh = useCallback(() => {
    api.db
      .list()
      .then((res) => {
        setDatabases(res.databases);
        const current = res.databases.find((d) => d.name === currentDatabase);
        if (current?.database_type) {
          onSelectDatabase(currentDatabase, current.database_type);
        }
      })
      .catch(() => {});
  }, [currentDatabase, onSelectDatabase]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const handleChange = (e: React.ChangeEvent<HTMLSelectElement>) => {
    const name = e.target.value;
    const db = databases.find((d) => d.name === name);
    onSelectDatabase(name, db?.database_type);
  };

  const hasCurrent = databases.some((d) => d.name === currentDatabase);

  return (
    <>
      <select
        className={styles.select}
        value={hasCurrent ? currentDatabase : ""}
        onChange={handleChange}
        aria-label="Active database"
        disabled={databases.length === 0}
      >
        {databases.length === 0 && <option value="">No databases</option>}
        {databases.map((db) => (
          <option key={db.name} value={db.name}>
            {db.name} ({db.node_count.toLocaleString()}n · {db.edge_count.toLocaleString()}e)
          </option>
        ))}
      </select>

      <div className={styles.actions}>
        <button
          type="button"
          className={styles.actionButton}
          onClick={() => setDialogOpen(true)}
        >
          + New database
        </button>
        {hasCurrent && (
          <Link
            to={`/databases/${encodeURIComponent(currentDatabase)}`}
            className={styles.actionLink}
            title={`Manage ${currentDatabase}`}
          >
            Manage →
          </Link>
        )}
      </div>

      <CreateDatabaseDialog
        open={dialogOpen}
        onClose={() => setDialogOpen(false)}
        onCreated={refresh}
      />
    </>
  );
}
