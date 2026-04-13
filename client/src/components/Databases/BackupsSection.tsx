import { useCallback, useEffect, useState } from "react";
import { api, GrafeoApiError } from "../../api/client";
import type { BackupEntry, DatabaseSummary } from "../../types/api";
import btn from "../../styles/buttons.module.css";
import CreateBackupDialog from "./CreateBackupDialog";
import EpochRestoreDialog from "./EpochRestoreDialog";
import RestoreDialog from "./RestoreDialog";
import styles from "./BackupsSection.module.css";

interface Props {
  database: string;
  /** Called after a successful backup or restore so the parent page can
   *  refetch stats. */
  onMutated?: () => void;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024)
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

function formatDate(iso: string): string {
  try {
    return new Date(iso).toLocaleString();
  } catch {
    return iso;
  }
}

function filenameKey(filename: string): string {
  const match = filename.match(/_(\d+)\.grafeo$/);
  return match ? `#${match[1]}` : filename.replace(/\.grafeo$/, "");
}

export default function BackupsSection({ database, onMutated }: Props) {
  const [backups, setBackups] = useState<BackupEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const [submittingCreate, setSubmittingCreate] = useState(false);
  const [restoring, setRestoring] = useState<BackupEntry | null>(null);
  const [databases, setDatabases] = useState<DatabaseSummary[]>([]);
  const [toast, setToast] = useState<string | null>(null);
  const [epochRestoring, setEpochRestoring] = useState(false);
  const [epochError, setEpochError] = useState<string | null>(null);
  const [submittingEpoch, setSubmittingEpoch] = useState(false);

  const refresh = useCallback(() => {
    setLoading(true);
    api.backup
      .list(database)
      .then(setBackups)
      .catch(() => setBackups([]))
      .finally(() => setLoading(false));
  }, [database]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    api.db
      .list()
      .then((r) => setDatabases(r.databases))
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (!toast) return;
    const id = window.setTimeout(() => setToast(null), 2500);
    return () => window.clearTimeout(id);
  }, [toast]);

  const handleCreate = useCallback(
    async (label: string | undefined) => {
      setCreateError(null);
      setSubmittingCreate(true);
      try {
        const created = await api.backup.create(database, label);
        const displayName = label ?? filenameKey(created.filename);
        setToast(`Backup created: ${displayName}`);
        setCreating(false);
        refresh();
        onMutated?.();
      } catch (err) {
        setCreateError(
          err instanceof GrafeoApiError ? err.detail : String(err),
        );
      } finally {
        setSubmittingCreate(false);
      }
    },
    [database, refresh, onMutated],
  );

  const handleRestoreToEpoch = useCallback(
    async (epoch: number) => {
      setEpochError(null);
      setSubmittingEpoch(true);
      try {
        await api.backup.restoreToEpoch(database, epoch);
        setEpochRestoring(false);
        setToast(`Restored ${database} to epoch ${epoch}`);
        refresh();
        onMutated?.();
      } catch (err) {
        setEpochError(
          err instanceof GrafeoApiError ? err.detail : String(err),
        );
      } finally {
        setSubmittingEpoch(false);
      }
    },
    [database, refresh, onMutated],
  );

  const handleDelete = useCallback(
    async (b: BackupEntry) => {
      const display = b.label ?? filenameKey(b.filename);
      if (!confirm(`Delete backup "${display}"?`)) return;
      try {
        await api.backup.remove(database, b.filename);
        refresh();
      } catch (err) {
        alert(`Delete failed: ${err}`);
      }
    },
    [database, refresh],
  );

  const handleRestore = useCallback(
    async (targetDb: string, backup: string) => {
      try {
        await api.backup.restore(targetDb, backup, database);
        setRestoring(null);
        setToast(`Restored ${targetDb} from ${filenameKey(backup)}`);
        refresh();
        onMutated?.();
      } catch (err) {
        alert(`Restore failed: ${err}`);
      }
    },
    [database, refresh, onMutated],
  );

  return (
    <section className={styles.section}>
      <div className={styles.header}>
        <h3 className={styles.heading}>Backups</h3>
        <div className={styles.headerActions}>
          <button
            type="button"
            className={btn.link}
            onClick={() => {
              setEpochError(null);
              setEpochRestoring(true);
            }}
            disabled={backups.length === 0}
            title={
              backups.length === 0
                ? "No backups available to restore from"
                : "Restore to a point in time"
            }
          >
            Restore to epoch…
          </button>
          <button
            type="button"
            className={btn.secondary}
            onClick={() => {
              setCreateError(null);
              setCreating(true);
            }}
          >
            + New backup
          </button>
        </div>
      </div>

      {toast && <div className={styles.toast}>{toast}</div>}

      {loading ? (
        <div className={styles.empty}>Loading…</div>
      ) : backups.length === 0 ? (
        <div className={styles.empty}>
          No backups yet. Create one with the button above.
        </div>
      ) : (
        <div className={styles.tableWrap}>
          <table className={styles.table}>
            <thead>
              <tr>
                <th>Name</th>
                <th>Kind</th>
                <th>Created</th>
                <th>Size</th>
                <th>Epoch</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {backups.map((b) => {
                const display = b.label ?? filenameKey(b.filename);
                return (
                  <tr key={b.filename}>
                    <td className={styles.nameCell} title={b.filename}>
                      <span
                        className={b.label ? styles.label : styles.sequence}
                      >
                        {display}
                      </span>
                    </td>
                    <td>{b.kind}</td>
                    <td>{formatDate(b.created_at)}</td>
                    <td>{formatBytes(b.size_bytes)}</td>
                    <td>{b.end_epoch}</td>
                    <td className={styles.rowActions}>
                      <a
                        href={api.backup.downloadUrl(database, b.filename)}
                        className={btn.link}
                        download
                      >
                        Download
                      </a>
                      <button
                        type="button"
                        className={btn.link}
                        onClick={() => setRestoring(b)}
                        disabled={b.kind !== "full"}
                        title={
                          b.kind === "full"
                            ? "Restore this exact snapshot"
                            : "Incremental backups can only be restored via Restore to epoch"
                        }
                      >
                        Restore
                      </button>
                      <button
                        type="button"
                        className={`${btn.link} ${btn.danger}`}
                        onClick={() => handleDelete(b)}
                      >
                        Delete
                      </button>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}

      {creating && (
        <CreateBackupDialog
          database={database}
          submitting={submittingCreate}
          error={createError}
          onCancel={() => setCreating(false)}
          onSubmit={handleCreate}
        />
      )}

      {restoring && (
        <RestoreDialog
          backup={restoring}
          databases={databases.map((d) => d.name)}
          defaultTarget={database}
          onRestore={handleRestore}
          onCancel={() => setRestoring(null)}
        />
      )}

      {epochRestoring && (
        <EpochRestoreDialog
          database={database}
          backups={backups}
          submitting={submittingEpoch}
          error={epochError}
          onCancel={() => setEpochRestoring(false)}
          onSubmit={handleRestoreToEpoch}
        />
      )}
    </section>
  );
}
