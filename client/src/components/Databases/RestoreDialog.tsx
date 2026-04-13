import { useState } from "react";
import type { BackupEntry } from "../../types/api";
import styles from "./RestoreDialog.module.css";

interface Props {
  backup: BackupEntry;
  databases: string[];
  defaultTarget: string;
  onRestore: (targetDb: string, backupFilename: string) => void;
  onCancel: () => void;
}

export default function RestoreDialog({
  backup,
  databases,
  defaultTarget,
  onRestore,
  onCancel,
}: Props) {
  const [target, setTarget] = useState(defaultTarget);
  const [confirming, setConfirming] = useState(false);

  const handleRestore = () => {
    if (!confirming) {
      setConfirming(true);
      return;
    }
    onRestore(target, backup.filename);
  };

  return (
    <div className={styles.overlay} onClick={onCancel}>
      <div className={styles.dialog} onClick={(e) => e.stopPropagation()}>
        <h3 className={styles.title}>Restore from backup</h3>
        <p className={styles.detail}>
          Restoring <strong>{backup.filename}</strong> will replace the target
          database's data. A safety backup is created automatically.
        </p>

        <label className={styles.label}>
          Target database
          <select
            className={styles.select}
            value={target}
            onChange={(e) => { setTarget(e.target.value); setConfirming(false); }}
          >
            {databases.map((db) => (
              <option key={db} value={db}>{db}</option>
            ))}
          </select>
        </label>

        <div className={styles.actions}>
          <button className={styles.cancel} onClick={onCancel}>Cancel</button>
          <button
            className={confirming ? styles.confirmDanger : styles.confirm}
            onClick={handleRestore}
          >
            {confirming ? `Confirm restore to ${target}` : "Restore"}
          </button>
        </div>
      </div>
    </div>
  );
}
