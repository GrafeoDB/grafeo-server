import { useState } from "react";
import { Button, Dialog } from "@grafeo-db/ui";
import { fmt } from "../shell/format";

const LABEL = /^[A-Za-z0-9_-]{1,32}$/;

type BackupDialogProps = {
  open: boolean;
  database: string;
  epoch: number | undefined;
  busy: boolean;
  error: string | null;
  onClose(): void;
  onBackup(label: string | undefined): void;
};

/** Back up now: a full snapshot, with an optional label. Its form starts empty each time it opens. */
export function BackupDialog(props: BackupDialogProps) {
  return props.open ? <OpenBackupDialog {...props} /> : null;
}

function OpenBackupDialog({
  database,
  epoch,
  busy,
  error,
  onClose,
  onBackup,
}: BackupDialogProps) {
  const [label, setLabel] = useState("");
  const valid = label === "" || LABEL.test(label);
  const submit = () => valid && !busy && onBackup(label === "" ? undefined : label);
  return (
    <Dialog
      open
      onClose={onClose}
      title={`Back up ${database}`}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={!valid || busy} onClick={submit}>
            {busy ? "Backing up…" : "Back up now"}
          </Button>
        </>
      }
    >
      <p className="gu-dialog-text">
        Takes a full snapshot of <strong>{database}</strong>
        {epoch !== undefined ? (
          <>
            {" "}
            at epoch <strong className="st-num">{fmt(epoch)}</strong>
          </>
        ) : null}
        .
      </p>
      <form
        className="gu-field"
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <label htmlFor="st-backup-label">
          Label <span className="st-muted">(optional)</span>
        </label>
        <input
          id="st-backup-label"
          className="gu-input"
          placeholder="pre-migration"
          maxLength={32}
          value={label}
          aria-invalid={!valid || undefined}
          onChange={(e) => setLabel(e.target.value)}
        />
        <span className="st-hint">
          Letters, digits, - and _, up to 32 characters. Without a label the backup gets the next number.
        </span>
      </form>
      {error ? (
        <p className="st-error" role="alert">
          {error}
        </p>
      ) : null}
    </Dialog>
  );
}

type RestoreDialogProps = {
  open: boolean;
  backupName: string;
  databases: string[];
  defaultTarget: string;
  busy: boolean;
  error: string | null;
  onClose(): void;
  onRestore(target: string): void;
};

/** Restore from a backup into a database of choice (the backup's own by default). */
export function RestoreDialog(props: RestoreDialogProps) {
  return props.open ? <OpenRestoreDialog {...props} /> : null;
}

function OpenRestoreDialog({
  backupName,
  databases,
  defaultTarget,
  busy,
  error,
  onClose,
  onRestore,
}: RestoreDialogProps) {
  const [target, setTarget] = useState(defaultTarget);
  return (
    <Dialog
      open
      onClose={onClose}
      title={`Restore from ${backupName}`}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="danger" disabled={busy} onClick={() => onRestore(target)}>
            {busy ? "Restoring…" : "Restore"}
          </Button>
        </>
      }
    >
      <p className="gu-dialog-text">
        Replaces the data in the target database with this snapshot. A safety backup of the target is taken first.
      </p>
      <div className="gu-field">
        <label htmlFor="st-restore-target">Target database</label>
        <select id="st-restore-target" className="gu-input" value={target} onChange={(e) => setTarget(e.target.value)}>
          {databases.map((name) => (
            <option key={name} value={name}>
              {name}
            </option>
          ))}
        </select>
      </div>
      {error ? (
        <p className="st-error" role="alert">
          {error}
        </p>
      ) : null}
    </Dialog>
  );
}
