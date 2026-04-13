import { useCallback, useState } from "react";
import { useNavigate } from "react-router-dom";
import { api, GrafeoApiError } from "../../api/client";
import btn from "../../styles/buttons.module.css";
import styles from "./DangerZone.module.css";

interface Props {
  database: string;
}

export default function DangerZone({ database }: Props) {
  const navigate = useNavigate();
  const [confirmName, setConfirmName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [deleting, setDeleting] = useState(false);

  const canDelete = confirmName === database && database !== "default";

  const handleDelete = useCallback(async () => {
    setError(null);
    setDeleting(true);
    try {
      await api.db.delete(database);
      navigate("/databases", { replace: true });
    } catch (err) {
      setError(err instanceof GrafeoApiError ? err.detail : String(err));
      setDeleting(false);
    }
  }, [database, navigate]);

  return (
    <section className={styles.section}>
      <h3 className={styles.heading}>Danger zone</h3>
      <div className={styles.card}>
        <div className={styles.text}>
          <div className={styles.label}>Delete database</div>
          <div className={styles.description}>
            Permanently destroys <strong>{database}</strong> and all of its
            data. This cannot be undone. Type the database name below to
            enable the delete button.
          </div>
        </div>

        {database === "default" ? (
          <div className={styles.disabled}>
            The default database cannot be deleted.
          </div>
        ) : (
          <div className={styles.actions}>
            <input
              type="text"
              className={styles.input}
              placeholder={`Type "${database}" to confirm`}
              value={confirmName}
              onChange={(e) => setConfirmName(e.target.value)}
              disabled={deleting}
              aria-label="Confirm database name"
            />
            <button
              type="button"
              className={`${btn.secondary} ${styles.deleteButton}`}
              onClick={handleDelete}
              disabled={!canDelete || deleting}
            >
              {deleting ? "Deleting…" : "Delete database"}
            </button>
          </div>
        )}
        {error && <div className={styles.error}>{error}</div>}
      </div>
    </section>
  );
}
