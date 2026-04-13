import { useState, useCallback } from "react";
import { api } from "../../api/client";
import styles from "./CreateTokenDialog.module.css";

interface Props {
  databases: string[];
  onCreated: () => void;
  onCancel: () => void;
}

export default function CreateTokenDialog({ databases, onCreated, onCancel }: Props) {
  const [name, setName] = useState("");
  const [role, setRole] = useState("read-only");
  const [selectedDbs, setSelectedDbs] = useState<string[]>([]);
  const [plaintext, setPlaintext] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [copied, setCopied] = useState(false);

  const handleCreate = useCallback(async () => {
    if (!name.trim()) {
      setError("Name is required");
      return;
    }
    setCreating(true);
    setError(null);
    try {
      const resp = await api.tokens.create({
        name: name.trim(),
        scope: { role, databases: selectedDbs },
      });
      if (resp.token) {
        setPlaintext(resp.token);
      } else {
        onCreated();
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setCreating(false);
    }
  }, [name, role, selectedDbs, onCreated]);

  const handleCopy = useCallback(() => {
    if (plaintext) {
      navigator.clipboard.writeText(plaintext).then(() => setCopied(true));
    }
  }, [plaintext]);

  const toggleDb = useCallback((db: string) => {
    setSelectedDbs((prev) =>
      prev.includes(db) ? prev.filter((d) => d !== db) : [...prev, db],
    );
  }, []);

  // After showing the token, user dismisses and we notify parent
  if (plaintext) {
    return (
      <div className={styles.overlay} onClick={() => onCreated()}>
        <div className={styles.dialog} onClick={(e) => e.stopPropagation()}>
          <h3 className={styles.title}>Token created</h3>
          <p className={styles.warning}>
            Copy this token now. It won't be shown again.
          </p>
          <div className={styles.tokenBox}>
            <code className={styles.tokenValue}>{plaintext}</code>
            <button className={styles.copyButton} onClick={handleCopy}>
              {copied ? "Copied" : "Copy"}
            </button>
          </div>
          <div className={styles.actions}>
            <button className={styles.done} onClick={() => onCreated()}>
              Done
            </button>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className={styles.overlay} onClick={onCancel}>
      <div className={styles.dialog} onClick={(e) => e.stopPropagation()}>
        <h3 className={styles.title}>Create API token</h3>

        <label className={styles.label}>
          Name
          <input
            className={styles.input}
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="e.g. ci-pipeline, alice-readonly"
            autoFocus
          />
        </label>

        <label className={styles.label}>
          Role
          <select className={styles.select} value={role} onChange={(e) => setRole(e.target.value)}>
            <option value="admin">Admin (full access)</option>
            <option value="read-write">Read-Write (query + mutate)</option>
            <option value="read-only">Read-Only (query only)</option>
          </select>
        </label>

        <div className={styles.label}>
          Databases
          <span className={styles.hint}>Leave empty for access to all databases</span>
          <div className={styles.dbGrid}>
            {databases.map((db) => (
              <label key={db} className={styles.dbCheckbox}>
                <input
                  type="checkbox"
                  checked={selectedDbs.includes(db)}
                  onChange={() => toggleDb(db)}
                />
                {db}
              </label>
            ))}
          </div>
        </div>

        {error && <div className={styles.error}>{error}</div>}

        <div className={styles.actions}>
          <button className={styles.cancel} onClick={onCancel}>Cancel</button>
          <button className={styles.create} onClick={handleCreate} disabled={creating}>
            {creating ? "Creating..." : "Create Token"}
          </button>
        </div>
      </div>
    </div>
  );
}
