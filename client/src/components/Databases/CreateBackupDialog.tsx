import { useEffect, useRef, useState } from "react";
import btn from "../../styles/buttons.module.css";
import styles from "./CreateBackupDialog.module.css";

interface Props {
  database: string;
  submitting: boolean;
  error: string | null;
  onSubmit: (label: string | undefined) => void;
  onCancel: () => void;
}

const LABEL_PATTERN = /^[A-Za-z0-9_-]{1,32}$/;

export default function CreateBackupDialog({
  database,
  submitting,
  error,
  onSubmit,
  onCancel,
}: Props) {
  const [label, setLabel] = useState("");
  const inputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const trimmed = label.trim();
  const labelValid = trimmed.length === 0 || LABEL_PATTERN.test(trimmed);

  const handleSubmit = (e?: React.FormEvent) => {
    e?.preventDefault();
    if (!labelValid || submitting) return;
    onSubmit(trimmed.length > 0 ? trimmed : undefined);
  };

  const handleOverlayClick = (e: React.MouseEvent) => {
    if (e.target === e.currentTarget && !submitting) onCancel();
  };

  return (
    <div className={styles.overlay} onClick={handleOverlayClick}>
      <form className={styles.dialog} onSubmit={handleSubmit}>
        <h3 className={styles.title}>New backup</h3>
        <p className={styles.description}>
          Creates a point-in-time snapshot of <strong>{database}</strong>. The
          database stays available during the backup.
        </p>

        <label className={styles.field}>
          <span className={styles.label}>
            Label <span className={styles.optional}>(optional)</span>
          </span>
          <input
            ref={inputRef}
            type="text"
            className={styles.input}
            placeholder="e.g. pre-migration, weekly, before-import"
            value={label}
            onChange={(e) => setLabel(e.target.value)}
            disabled={submitting}
            maxLength={32}
            aria-invalid={!labelValid}
          />
          {!labelValid && (
            <span className={styles.fieldError}>
              Letters, digits, '-', and '_' only. Max 32 characters.
            </span>
          )}
          {labelValid && (
            <span className={styles.hint}>
              Press Enter to submit. Leave empty for an unlabeled backup.
            </span>
          )}
        </label>

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
            disabled={!labelValid || submitting}
          >
            {submitting ? "Creating…" : "Create backup"}
          </button>
        </div>
      </form>
    </div>
  );
}
