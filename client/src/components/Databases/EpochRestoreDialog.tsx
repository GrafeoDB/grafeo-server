import { useEffect, useRef, useState } from "react";
import type { BackupEntry } from "../../types/api";
import btn from "../../styles/buttons.module.css";
import styles from "./EpochRestoreDialog.module.css";

interface Props {
  database: string;
  backups: BackupEntry[];
  submitting: boolean;
  error: string | null;
  onSubmit: (epoch: number) => void;
  onCancel: () => void;
}

/**
 * Restore a database to an arbitrary epoch by replaying the backup chain
 * up to that point. Distinct from the row-level "Restore" action, which
 * restores from a single full backup exactly.
 */
export default function EpochRestoreDialog({
  database,
  backups,
  submitting,
  error,
  onSubmit,
  onCancel,
}: Props) {
  // Highest end_epoch across all backups is the most recent restorable point.
  const maxEpoch = backups.reduce(
    (m, b) => Math.max(m, b.end_epoch),
    0,
  );
  const [epochText, setEpochText] = useState(String(maxEpoch));
  const inputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  const parsed = Number(epochText);
  const valid =
    Number.isInteger(parsed) && parsed >= 0 && epochText.trim().length > 0;

  const handleSubmit = (e?: React.FormEvent) => {
    e?.preventDefault();
    if (!valid || submitting) return;
    onSubmit(parsed);
  };

  const handleOverlayClick = (e: React.MouseEvent) => {
    if (e.target === e.currentTarget && !submitting) onCancel();
  };

  // Sort ascending so the list reads like a timeline.
  const timeline = [...backups].sort((a, b) => a.end_epoch - b.end_epoch);

  return (
    <div className={styles.overlay} onClick={handleOverlayClick}>
      <form className={styles.dialog} onSubmit={handleSubmit}>
        <h3 className={styles.title}>Restore to point in time</h3>
        <p className={styles.description}>
          Replays the backup chain for <strong>{database}</strong> up to the
          target epoch. The current database state is lost — the engine
          reopens from the restored file after the swap.
        </p>

        <label className={styles.field}>
          <span className={styles.label}>Target epoch</span>
          <input
            ref={inputRef}
            type="number"
            className={styles.input}
            value={epochText}
            onChange={(e) => setEpochText(e.target.value)}
            min={0}
            max={maxEpoch}
            step={1}
            disabled={submitting}
            aria-invalid={!valid}
          />
          {!valid && (
            <span className={styles.fieldError}>
              Must be a non-negative integer.
            </span>
          )}
        </label>

        {timeline.length > 0 && (
          <div className={styles.timeline}>
            <span className={styles.timelineLabel}>Known backups</span>
            <ul className={styles.timelineList}>
              {timeline.map((b) => {
                const name = b.label ?? b.filename.replace(/\.grafeo$/, "");
                return (
                  <li key={b.filename} className={styles.timelineItem}>
                    <button
                      type="button"
                      className={styles.timelinePill}
                      onClick={() => setEpochText(String(b.end_epoch))}
                      title={`Use epoch ${b.end_epoch}`}
                      disabled={submitting}
                    >
                      <span className={styles.timelineKind}>{b.kind}</span>
                      <span className={styles.timelineName}>{name}</span>
                      <span className={styles.timelineEpoch}>
                        e{b.end_epoch}
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          </div>
        )}

        {error && <div className={styles.error}>{error}</div>}

        <div className={styles.actions}>
          <button
            type="button"
            className={btn.secondary}
            onClick={onCancel}
            disabled={submitting}
          >
            Cancel
          </button>
          <button
            type="submit"
            className={btn.primary}
            disabled={!valid || submitting}
          >
            {submitting ? "Restoring…" : `Restore to epoch ${parsed || 0}`}
          </button>
        </div>
      </form>
    </div>
  );
}
