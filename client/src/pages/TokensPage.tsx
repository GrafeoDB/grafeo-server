import { useEffect, useState } from "react";
import { Button, DataTable, Dialog, FORBIDDEN_NOTE, Page, UNAUTHENTICATED_NOTE, type DataColumn } from "@grafeo-db/ui";
import { api, GrafeoApiError, refusedKind } from "../api/client";
import type { TokenResponse } from "../types/api";
import { useServer } from "../context/ServerContext";
import { formatDate } from "../shell/format";
import { ActionError, Chip, useAsk } from "./common";

const ROLES = [
  { id: "read-only", gloss: "queries only" },
  { id: "read-write", gloss: "queries and changes" },
  { id: "admin", gloss: "everything" },
] as const;

function message(err: unknown): string {
  const kind = refusedKind(err);
  if (kind) return kind === "forbidden" ? FORBIDDEN_NOTE : UNAUTHENTICATED_NOTE;
  return err instanceof GrafeoApiError ? err.detail : err instanceof Error ? err.message : String(err);
}

export default function TokensPage() {
  const { databases, tokens, refreshTokens: reload } = useServer();
  const ask = useAsk();

  const [creating, setCreating] = useState(false);
  const [created, setCreated] = useState<TokenResponse | null>(null);
  const [copied, setCopied] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  useEffect(() => {
    if (!copied) return;
    const id = window.setTimeout(() => setCopied(false), 2000);
    return () => window.clearTimeout(id);
  }, [copied]);

  const refused = tokens.state === "refused" ? tokens.refused : undefined;
  const unconfigured = tokens.state === "ready" && tokens.data === null;
  const tokenRows = tokens.state === "ready" ? (tokens.data ?? []) : [];
  // Ticking no database means all of them, so the list must be known before a token is made.
  const dbsReady = databases.state === "ready";

  const columns: DataColumn<TokenResponse>[] = [
    { key: "name", header: "Name", render: (t) => <span className="st-strong">{t.name}</span> },
    {
      key: "role",
      header: "Role",
      render: (t) => <Chip tone={t.scope.role === "admin" ? "strong" : undefined}>{t.scope.role}</Chip>,
    },
    {
      key: "databases",
      header: "Databases",
      render: (t) =>
        t.scope.databases.length ? (
          <span className="gu-mono">{t.scope.databases.join(", ")}</span>
        ) : (
          <span className="st-muted">all databases</span>
        ),
    },
    { key: "created", header: "Created", align: "end", render: (t) => formatDate(t.created_at) },
    {
      key: "actions",
      header: "",
      render: (t) => (
        <span className="st-row-actions">
          <Button
            variant="ghost"
            size="sm"
            onClick={() =>
              ask.open({
                title: `Revoke ${t.name}?`,
                confirmLabel: "Revoke token",
                danger: true,
                body: "Requests with this token fail from now on. A new token is needed to restore access.",
                onConfirm: () =>
                  api.tokens
                    .delete(t.id)
                    .then(() => {
                      setActionError(null);
                      if (created?.id === t.id) setCreated(null);
                      reload();
                    })
                    .catch((err: unknown) => setActionError(message(err))),
              })
            }
          >
            Revoke
          </Button>
        </span>
      ),
    },
  ];

  return (
    <Page
      title="API tokens"
      meta="Bearer tokens for the HTTP API. A token's role and databases limit what it can do."
      actions={
        <Button
          variant="primary"
          disabled={!!refused || unconfigured || !dbsReady}
          title={!dbsReady ? "The database list has not loaded, so a token's databases can't be chosen yet." : undefined}
          onClick={() => setCreating(true)}
        >
          New token
        </Button>
      }
    >
      <ActionError message={actionError} />
      {created?.token ? (
        <div className="st-reveal" role="region" aria-label="New token">
          <div className="st-reveal-head">
            <strong>{created.name}</strong>
            <Chip>{created.scope.role}</Chip>
            <Chip>{created.scope.databases.length ? created.scope.databases.join(", ") : "all databases"}</Chip>
            <span className="st-spacer" />
            <Button variant="ghost" size="sm" onClick={() => setCreated(null)}>
              Done
            </Button>
          </div>
          <p>Copy the token now. It is shown only this once.</p>
          <div className="st-secret">
            <code>{created.token}</code>
            <Button
              onClick={() => {
                navigator.clipboard
                  ?.writeText(created.token ?? "")
                  .then(() => setCopied(true))
                  .catch(() => setActionError("The browser did not allow copying; select the token and copy it."));
              }}
            >
              {copied ? "Copied" : "Copy"}
            </Button>
          </div>
          <p className="st-usage">
            Send it in a header: <code>Authorization: Bearer &lt;token&gt;</code>
          </p>
        </div>
      ) : null}
      <div className="st-after-reveal">
        {unconfigured ? (
          <p className="st-hint" role="status">
            Token management is not configured on this server. It keeps tokens when it checks credentials and runs
            with --data-dir or --token-store-path.
          </p>
        ) : tokens.state === "failed" ? (
          <p className="st-error" role="alert">
            The tokens did not load: {tokens.message}
          </p>
        ) : (
          <DataTable
            label="API tokens"
            columns={columns}
            rows={tokenRows}
            rowKey={(t) => t.id}
            isMarked={(t) => created?.id === t.id}
            refused={refused}
            empty={tokens.state === "loading" ? undefined : "No tokens yet. New token creates one and shows it once."}
          />
        )}
      </div>
      <NewTokenDialog
        open={creating}
        databases={databases.state === "ready" ? databases.data.map((d) => d.name) : []}
        onClose={() => setCreating(false)}
        onCreated={(token) => {
          setCreating(false);
          setCreated(token);
          setActionError(null);
          reload();
        }}
      />
      {ask.dialog}
    </Page>
  );
}

type NewTokenDialogProps = {
  open: boolean;
  databases: string[];
  onClose(): void;
  onCreated(token: TokenResponse): void;
};

/** New token: mounted only while open, so each opening starts from an empty form. */
function NewTokenDialog(props: NewTokenDialogProps) {
  return props.open ? <OpenNewTokenDialog {...props} /> : null;
}

function OpenNewTokenDialog({ databases, onClose, onCreated }: NewTokenDialogProps) {
  const [name, setName] = useState("");
  const [role, setRole] = useState<string>("read-only");
  const [picked, setPicked] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    if (!name.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      onCreated(await api.tokens.create({ name: name.trim(), scope: { role, databases: picked } }));
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open
      onClose={onClose}
      title="New token"
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={!name.trim() || busy} onClick={() => void submit()}>
            {busy ? "Creating…" : "Create token"}
          </Button>
        </>
      }
    >
      <form
        className="st-fields"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <div className="gu-field">
          <label htmlFor="st-token-name">Name</label>
          <input
            id="st-token-name"
            className="gu-input"
            placeholder="ci-pipeline"
            value={name}
            autoFocus
            onChange={(e) => setName(e.target.value)}
          />
        </div>
        <div className="gu-field">
          <label htmlFor="st-token-role">Role</label>
          <select id="st-token-role" className="gu-input" value={role} onChange={(e) => setRole(e.target.value)}>
            {ROLES.map((r) => (
              <option key={r.id} value={r.id}>
                {r.id}: {r.gloss}
              </option>
            ))}
          </select>
        </div>
        <fieldset className="gu-field st-group">
          <legend className="st-field-label">Databases</legend>
          <span className="st-hint">None ticked gives access to all of them.</span>
          <div className="st-checks">
            {databases.map((db) => (
              <label className="st-check" key={db}>
                <input
                  type="checkbox"
                  checked={picked.includes(db)}
                  onChange={(e) =>
                    setPicked((prev) => (e.target.checked ? [...prev, db] : prev.filter((p) => p !== db)))
                  }
                />
                {db}
              </label>
            ))}
          </div>
        </fieldset>
        {error ? (
          <p className="st-error" role="alert">
            {error}
          </p>
        ) : null}
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
}
