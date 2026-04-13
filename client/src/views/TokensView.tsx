import { useCallback, useEffect, useState } from "react";
import { api, GrafeoApiError } from "../api/client";
import type { DatabaseSummary } from "../types/api";
import TokenPanel from "../components/Tokens/TokenPanel";
import CreateTokenDialog from "../components/Tokens/CreateTokenDialog";
import btn from "../styles/buttons.module.css";
import styles from "./TokensView.module.css";

export default function TokensView() {
  const [creating, setCreating] = useState(false);
  const [refreshKey, setRefreshKey] = useState(0);
  const [databases, setDatabases] = useState<DatabaseSummary[] | null>(null);
  const [dbLoadError, setDbLoadError] = useState<string | null>(null);

  const loadDatabases = useCallback(() => {
    setDbLoadError(null);
    api.db
      .list()
      .then((r) => setDatabases(r.databases))
      .catch((err) => {
        // Surface the failure rather than silently falling back to an
        // empty list. An empty `databases` array gets interpreted as
        // "all databases" by the token scope logic, so swallowing this
        // would let the user accidentally create an unrestricted token
        // when they intended a scoped one.
        setDatabases(null);
        setDbLoadError(
          err instanceof GrafeoApiError ? err.detail : String(err),
        );
      });
  }, []);

  useEffect(() => {
    loadDatabases();
  }, [loadDatabases]);

  const handleCreated = useCallback(() => {
    setCreating(false);
    setRefreshKey((k) => k + 1);
  }, []);

  const canCreate = databases !== null && dbLoadError === null;

  return (
    <div className={styles.page}>
      <div className={styles.header}>
        <h2 className={styles.heading}>API tokens</h2>
        <button
          type="button"
          className={btn.secondary}
          onClick={() => setCreating(true)}
          disabled={!canCreate}
          title={
            canCreate
              ? undefined
              : "Can't load the database list — retry before creating a token"
          }
        >
          + New token
        </button>
      </div>
      {dbLoadError && (
        <div className={styles.error}>
          Couldn't load databases for scoping: {dbLoadError}.{" "}
          <button
            type="button"
            className={styles.retry}
            onClick={loadDatabases}
          >
            Retry
          </button>
        </div>
      )}
      <div className={styles.panel}>
        <TokenPanel key={refreshKey} />
      </div>
      {creating && databases !== null && (
        <CreateTokenDialog
          databases={databases.map((d) => d.name)}
          onCreated={handleCreated}
          onCancel={() => setCreating(false)}
        />
      )}
    </div>
  );
}
