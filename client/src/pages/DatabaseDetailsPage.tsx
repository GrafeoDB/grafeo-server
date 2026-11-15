import { useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import {
  Button,
  DataTable,
  FORBIDDEN_NOTE,
  Page,
  PageSection,
  StateNote,
  UNAUTHENTICATED_NOTE,
  type DataColumn,
} from "@grafeo-db/ui";
import { api, GrafeoApiError, refusedKind } from "../api/client";
import type { BackupEntry, SectionTierInfo } from "../types/api";
import { useServer } from "../context/ServerContext";
import { useLoaded } from "../hooks/useLoaded";
import { forgetLabelStyles } from "../labelStyles";
import { bytesText, fmt, formatDate, modelName } from "../shell/format";
import { ActionError, Chip, Stats, bytesStat, useAsk, type StatItem } from "./common";
import { BackupDialog, RestoreDialog } from "./BackupDialogs";

function message(err: unknown): string {
  const kind = refusedKind(err);
  if (kind) return kind === "forbidden" ? FORBIDDEN_NOTE : UNAUTHENTICATED_NOTE;
  return err instanceof GrafeoApiError ? err.detail : err instanceof Error ? err.message : String(err);
}

/** A backup's short name: its label, else its sequence number (`movies_3.grafeo` is `#3`). */
function backupName(b: BackupEntry): string {
  if (b.label) return b.label;
  const match = b.filename.match(/_(\d+)\.grafeo$/);
  return match ? `#${match[1]}` : b.filename.replace(/\.grafeo$/, "");
}

/** A figure not there yet: still loading, or unknown when its request failed (said above the sections). */
function pending(loaded: { state: string }): string {
  return loaded.state === "loading" ? "…" : "unknown";
}

const TIER: Record<SectionTierInfo["tier"], { tone: "ram" | "disk" | "none"; name: string }> = {
  in_memory: { tone: "ram", name: "In RAM" },
  on_disk: { tone: "disk", name: "On disk" },
  uninitialized: { tone: "none", name: "Not loaded" },
  unknown: { tone: "none", name: "Unknown" },
};

/** The server answers 400 "backup not configured" when it runs without a backup directory. */
function notConfigured(err: unknown): boolean {
  return err instanceof GrafeoApiError && err.status === 400 && err.detail.includes("backup not configured");
}

export default function DatabaseDetailsPage() {
  const { name = "" } = useParams();
  const navigate = useNavigate();
  const server = useServer();
  const ask = useAsk();
  const [tick, setTick] = useState(0);
  const refresh = () => setTick((t) => t + 1);
  const [notice, setNotice] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const summary = server.databases.state === "ready" ? server.databases.data.find((d) => d.name === name) : undefined;
  const [info] = useLoaded(() => api.db.info(name), [name, tick]);
  const [stats] = useLoaded(() => api.db.stats(name), [name, tick]);
  const [wal] = useLoaded(() => api.admin.walStatus(name), [name, tick]);
  const [backups, reloadBackups] = useLoaded(
    () =>
      api.backup.list(name).catch((err: unknown) => {
        if (notConfigured(err)) return null;
        throw err;
      }),
    [name, tick],
  );
  const persistent = summary?.persistent ?? (info.state === "ready" && info.data.persistent);
  const [tiers, reloadTiers] = useLoaded(
    () => (persistent ? api.admin.storageTiers(name).then((r) => r.tiers) : Promise.resolve([])),
    [name, persistent, tick],
  );

  const [backingUp, setBackingUp] = useState(false);
  const [backupBusy, setBackupBusy] = useState(false);
  const [backupError, setBackupError] = useState<string | null>(null);
  const [restoring, setRestoring] = useState<BackupEntry | null>(null);
  const [restoreBusy, setRestoreBusy] = useState(false);
  const [restoreError, setRestoreError] = useState<string | null>(null);
  // Untouched, the field holds the latest backup's epoch.
  const [epochInput, setEpochInput] = useState<string | null>(null);
  const [reloading, setReloading] = useState(false);
  const [epochBusy, setEpochBusy] = useState(false);

  const done = (text: string) => {
    setActionError(null);
    setNotice(text);
  };
  const failed = (err: unknown) => {
    setNotice(null);
    setActionError(message(err));
  };

  // Not found only when the list lacks it and the database itself does not answer: a database just created is on
  // the server before the refreshed list arrives.
  if (server.databases.state === "ready" && !summary && (info.state === "failed" || info.state === "refused")) {
    return (
      <Page
        breadcrumb={<DatabasesCrumb name={name} />}
        title={name}
        meta="This database is not on the server. It may have been deleted."
        actions={<Button onClick={() => navigate("/databases")}>All databases</Button>}
      />
    );
  }

  const refused = info.state === "refused" ? info.refused : server.databases.state === "refused" ? server.databases.refused : undefined;
  const type = summary?.database_type ?? (info.state === "ready" ? info.data.database_type : "lpg");
  const backupList = backups.state === "ready" ? backups.data : null;
  const configured = backups.state !== "ready" || backups.data !== null;
  const currentEpoch = wal.state === "ready" ? wal.data.current_epoch : undefined;
  const fullBackups = (backupList ?? []).slice().sort((a, b) => a.end_epoch - b.end_epoch);
  const reach = fullBackups.reduce((m, b) => Math.max(m, b.end_epoch), 0);
  const epochText = epochInput ?? (reach > 0 ? String(reach) : "");
  const epoch = Number(epochText);
  // Restoring to the epoch the database is already at changes nothing.
  const epochValid =
    epochText.trim() !== "" && Number.isInteger(epoch) && epoch >= 0 && epoch <= reach && epoch !== currentEpoch;

  // What did not load is said, never shown as empty figures or a WAL that is off.
  const loadFailures = [
    info.state === "failed" ? `the database (${info.message})` : null,
    stats.state === "failed" ? `its counts and sizes (${stats.message})` : null,
    wal.state === "failed" ? `its write-ahead log (${wal.message})` : null,
  ].filter((f): f is string => f !== null);

  let meta: string | undefined;
  if (info.state === "ready") {
    const i = info.data;
    meta = [
      i.memory_limit_bytes != null ? `Memory limit ${bytesText(i.memory_limit_bytes)}` : null,
      i.wal_enabled ? "WAL on" : "WAL off",
      i.backward_edges ? "backward edges on" : "backward edges off",
    ]
      .filter(Boolean)
      .join(" · ");
  }

  const backup = async (label: string | undefined) => {
    setBackupError(null);
    setBackupBusy(true);
    try {
      const created = await api.backup.create(name, label);
      setBackingUp(false);
      done(`Backup ${label ?? backupName(created)} of ${name} is on the server.`);
      reloadBackups();
    } catch (err) {
      setBackupError(message(err));
    } finally {
      setBackupBusy(false);
    }
  };

  const restore = async (target: string) => {
    if (!restoring) return;
    setRestoreError(null);
    setRestoreBusy(true);
    try {
      await api.backup.restore(target, restoring.filename, name);
      done(`${target} is restored from ${backupName(restoring)}.`);
      setRestoring(null);
      server.refreshDatabases();
      server.refreshSchema();
      refresh();
    } catch (err) {
      setRestoreError(message(err));
    } finally {
      setRestoreBusy(false);
    }
  };

  const backupColumns: DataColumn<BackupEntry>[] = [
    {
      key: "name",
      header: "Name",
      render: (b) => (
        <span className={b.label ? "st-strong" : "st-seq"} title={b.filename}>
          {backupName(b)}
        </span>
      ),
    },
    { key: "kind", header: "Kind", render: (b) => <Chip>{b.kind}</Chip> },
    { key: "created", header: "Created", align: "end", render: (b) => formatDate(b.created_at) },
    { key: "size", header: "Size", align: "end", render: (b) => bytesText(b.size_bytes) },
    { key: "epoch", header: "Epoch", align: "end", render: (b) => fmt(b.end_epoch) },
    {
      key: "actions",
      header: "",
      render: (b) => (
        <span className="st-row-actions">
          <a
            className="gu-button gu-button-ghost gu-button-sm"
            href={api.backup.downloadUrl(name, b.filename)}
            download={b.filename}
          >
            Download
          </a>
          <Button
            variant="ghost"
            size="sm"
            disabled={b.kind !== "full"}
            title={b.kind === "full" ? undefined : "An incremental backup restores through a point in time"}
            onClick={() => {
              setRestoreError(null);
              setRestoring(b);
            }}
          >
            Restore
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={() =>
              ask.open({
                title: `Delete backup ${backupName(b)}?`,
                confirmLabel: "Delete backup",
                danger: true,
                body: "Removes the file from the server. Restoring to a point in time can no longer use it.",
                onConfirm: () =>
                  api.backup
                    .remove(name, b.filename)
                    .then(() => {
                      done(`Backup ${backupName(b)} is deleted.`);
                      reloadBackups();
                    })
                    .catch(failed),
              })
            }
          >
            Delete
          </Button>
        </span>
      ),
    },
  ];

  const tierRows = tiers.state === "ready" ? tiers.data : [];
  const spilled = tierRows.filter((t) => t.tier === "on_disk").length;

  return (
    <Page
      breadcrumb={<DatabasesCrumb name={name} />}
      title={
        <>
          {name} <Chip>{modelName(type)}</Chip>
          {summary || info.state === "ready" ? <Chip>{persistent ? "Persistent" : "In memory"}</Chip> : null}
        </>
      }
      meta={meta}
      actions={
        <Button
          onClick={() => {
            server.selectDatabase(name);
            navigate("/");
          }}
        >
          Query {name}
        </Button>
      }
    >
      {notice ? (
        <p className="st-hint" role="status">
          {notice}
        </p>
      ) : null}
      <ActionError message={actionError} />
      {loadFailures.length ? (
        <div className="st-field-row">
          <p className="st-error" role="alert">
            Some details did not load: {loadFailures.join("; ")}.
          </p>
          <Button size="sm" onClick={refresh}>
            Retry
          </Button>
        </div>
      ) : null}

      <PageSection title="Overview" description="Counts and sizes as the engine reports them now.">
        {stats.state === "refused" || refused ? (
          <StateNote kind={stats.state === "refused" ? stats.refused : refused!} />
        ) : (
          <>
            <Stats
              items={(
                [
                  ["Nodes", "node_count"],
                  ["Edges", "edge_count"],
                  ["Labels", "label_count"],
                  ["Edge types", "edge_type_count"],
                  ["Property keys", "property_key_count"],
                  ["Indexes", "index_count"],
                ] as const
              ).map(([label, key]): StatItem => [label, stats.state === "ready" ? fmt(stats.data[key]) : pending(stats)])}
            />
            <Stats
              items={[
                stats.state === "ready" ? bytesStat("Memory", stats.data.memory_bytes) : ["Memory", pending(stats)],
                stats.state !== "ready"
                  ? ["Disk", pending(stats)]
                  : stats.data.disk_bytes == null
                    ? ["Disk", "none"]
                    : bytesStat("Disk", stats.data.disk_bytes),
                wal.state !== "ready"
                  ? ["WAL", pending(wal)]
                  : wal.data.enabled
                    ? bytesStat("WAL size", wal.data.size_bytes)
                    : ["WAL", "off"],
                wal.state !== "ready"
                  ? ["WAL pending", pending(wal)]
                  : wal.data.enabled
                    ? ["WAL pending", fmt(wal.data.record_count), wal.data.record_count === 1 ? "record" : "records"]
                    : ["WAL pending", "none"],
                ["Epoch", currentEpoch !== undefined ? fmt(currentEpoch) : pending(wal)],
                [
                  "Last checkpoint",
                  wal.state !== "ready" ? (
                    pending(wal)
                  ) : wal.data.last_checkpoint != null ? (
                    <span className="st-stat-date" key="d">
                      {formatDate(new Date(wal.data.last_checkpoint * 1000).toISOString())}
                    </span>
                  ) : (
                    "none"
                  ),
                ],
              ]}
            />
          </>
        )}
      </PageSection>

      {persistent ? (
        <PageSection
          title="Storage tiers"
          description="Where each section of the store lives. A section spills to disk when memory runs past the limit."
          action={
            <Button
              size="sm"
              disabled={reloading || spilled === 0}
              title="Bring spilled sections back into RAM, up to the server's memory target"
              onClick={() => {
                setReloading(true);
                api.admin
                  .reloadEligible(name)
                  .then((r) => {
                    done(
                      r.reloaded === 0
                        ? "Nothing reloaded: no spilled section fits within the memory target."
                        : `${r.reloaded === 1 ? "1 section is" : `${r.reloaded} sections are`} back in RAM.`,
                    );
                    reloadTiers();
                  })
                  .catch(failed)
                  .finally(() => setReloading(false));
              }}
            >
              {reloading ? "Reloading…" : "Reload spilled sections"}
            </Button>
          }
        >
          <div className="st-narrow">
            <DataTable
              label="Storage tiers"
              columns={[
                { key: "section", header: "Section", render: (t: SectionTierInfo) => <span className="gu-mono">{t.section}</span> },
                {
                  key: "tier",
                  header: "Tier",
                  render: (t: SectionTierInfo) => <Chip tone={TIER[t.tier]?.tone ?? "none"}>{TIER[t.tier]?.name ?? t.tier}</Chip>,
                },
              ]}
              rows={tierRows}
              rowKey={(t) => t.section}
              refused={tiers.state === "refused" ? tiers.refused : undefined}
              empty={
                tiers.state === "loading"
                  ? undefined
                  : tiers.state === "failed"
                    ? `The storage tiers did not load: ${tiers.message}`
                    : "No sections loaded yet."
              }
            />
          </div>
        </PageSection>
      ) : null}

      <PageSection
        title="Backups"
        description={
          configured
            ? `Snapshots of ${name} on the server. The database stays available while one is taken.`
            : "Backups are not configured on this server. It keeps them when it runs with --backup-dir."
        }
        action={
          configured ? (
            <Button
              size="sm"
              disabled={backups.state === "refused"}
              onClick={() => {
                setBackupError(null);
                setBackingUp(true);
              }}
            >
              Back up now
            </Button>
          ) : undefined
        }
      >
        {configured ? (
          backups.state === "failed" ? (
            <p className="st-error" role="alert">
              The backups did not load: {backups.message}
            </p>
          ) : (
            <DataTable
              label="Backups"
              columns={backupColumns}
              rows={backupList ?? []}
              rowKey={(b) => b.filename}
              refused={backups.state === "refused" ? backups.refused : undefined}
              empty={backups.state === "loading" ? undefined : `No backups of ${name} yet. Back up now makes the first one.`}
            />
          )
        ) : null}
      </PageSection>

      {configured && fullBackups.length > 0 ? (
        <PageSection
          title="Restore to a point in time"
          description={`Replays the backup chain up to an epoch. The current state of ${name} is replaced.`}
        >
          <div className="gu-field">
            <label htmlFor="st-epoch">Epoch</label>
            <div className="st-field-row">
              <input
                id="st-epoch"
                className="gu-input st-epoch"
                type="number"
                min={0}
                max={reach}
                value={epochText}
                onChange={(e) => setEpochInput(e.target.value)}
              />
              <Button
                disabled={!epochValid || epochBusy}
                onClick={() =>
                  ask.open({
                    title: `Restore ${name} to epoch ${fmt(epoch)}?`,
                    confirmLabel: "Restore",
                    danger: true,
                    body: (
                      <>
                        Replays the backups up to this epoch and swaps the result in. Changes to <strong>{name}</strong>{" "}
                        after it are lost.
                      </>
                    ),
                    onConfirm: () => {
                      setEpochBusy(true);
                      api.backup
                        .restoreToEpoch(name, epoch)
                        .then(() => {
                          done(`${name} is restored to epoch ${fmt(epoch)}.`);
                          server.refreshDatabases();
                          server.refreshSchema();
                          refresh();
                        })
                        .catch(failed)
                        .finally(() => setEpochBusy(false));
                    },
                  })
                }
              >
                {epochBusy ? "Restoring…" : epochValid ? `Restore to epoch ${fmt(epoch)}` : "Restore to epoch"}
              </Button>
            </div>
            <span className="st-hint">
              Backups reach epoch <span className="st-num">{fmt(reach)}</span>
              {currentEpoch !== undefined ? (
                <>
                  ; the database is at <span className="st-num">{fmt(currentEpoch)}</span>
                  {currentEpoch > reach ? ". Later changes are in no backup yet." : "."}
                </>
              ) : (
                "."
              )}
            </span>
            <div className="st-picks">
              {fullBackups.map((b) => (
                <button
                  type="button"
                  key={b.filename}
                  className={b.end_epoch === epoch ? "st-pick st-pick-on" : "st-pick"}
                  aria-pressed={b.end_epoch === epoch}
                  onClick={() => setEpochInput(String(b.end_epoch))}
                >
                  <b>{backupName(b)}</b>
                  {b.kind} · e{fmt(b.end_epoch)}
                </button>
              ))}
            </div>
          </div>
        </PageSection>
      ) : null}

      <PageSection title="Danger zone" tone="danger">
        <div className="st-danger-row">
          <p>
            <strong>Delete database.</strong>{" "}
            <span className="st-muted">
              {name === "default"
                ? "The default database cannot be deleted; the server always keeps it."
                : `Deletes ${name} and every node, edge and index in it. Backups stay on the server. This can't be undone.`}
            </span>
          </p>
          <Button
            variant="danger"
            disabled={name === "default"}
            onClick={() =>
              ask.open({
                title: `Delete ${name}?`,
                confirmLabel: "Delete database",
                danger: true,
                confirmText: name,
                body: (
                  <>
                    This deletes <strong>{name}</strong>
                    {stats.state === "ready" ? (
                      <>
                        {" "}
                        with its <strong>{fmt(stats.data.node_count)}</strong> nodes and{" "}
                        <strong>{fmt(stats.data.edge_count)}</strong> edges
                      </>
                    ) : null}
                    . Backups stay on the server. It can't be undone.
                  </>
                ),
                onConfirm: () =>
                  api.db
                    .delete(name)
                    .then(() => {
                      forgetLabelStyles(name);
                      server.refreshDatabases();
                      navigate("/databases", { replace: true });
                    })
                    .catch(failed),
              })
            }
          >
            Delete database
          </Button>
        </div>
      </PageSection>

      <BackupDialog
        open={backingUp}
        database={name}
        epoch={currentEpoch}
        busy={backupBusy}
        error={backupError}
        onClose={() => setBackingUp(false)}
        onBackup={(label) => void backup(label)}
      />
      <RestoreDialog
        open={restoring !== null}
        backupName={restoring ? backupName(restoring) : ""}
        databases={server.databases.state === "ready" ? server.databases.data.map((d) => d.name) : [name]}
        defaultTarget={name}
        busy={restoreBusy}
        error={restoreError}
        onClose={() => setRestoring(null)}
        onRestore={(target) => void restore(target)}
      />
      {ask.dialog}
    </Page>
  );
}

function DatabasesCrumb({ name }: { name: string }) {
  const navigate = useNavigate();
  return (
    <>
      <button type="button" className="st-link-button" onClick={() => navigate("/databases")}>
        Databases
      </button>
      <span>/</span>
      <span>{name}</span>
    </>
  );
}
