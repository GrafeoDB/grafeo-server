import { useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import { Button, DataTable, Page, type DataColumn } from "@grafeo-db/ui";
import { api } from "../api/client";
import type { DatabaseStatsResponse, DatabaseSummary } from "../types/api";
import { useServer } from "../context/ServerContext";
import { useLoaded } from "../hooks/useLoaded";
import { bytesText, count, fmt, modelName } from "../shell/format";
import { Chip, Stats, bytesStat } from "./common";
import NewDatabaseDialog from "./NewDatabaseDialog";

type Row = DatabaseSummary & { stats?: DatabaseStatsResponse };

export default function DatabasesPage() {
  const navigate = useNavigate();
  const server = useServer();
  const { databases, health } = server;
  const [creating, setCreating] = useState(false);
  const host = window.location.host;

  // Sizes come from each database's stats; a database whose stats the server refuses keeps its counts.
  const names = databases.state === "ready" ? databases.data.map((d) => d.name).join("\n") : "";
  const [stats] = useLoaded(async () => {
    if (!names) return new Map<string, DatabaseStatsResponse>();
    const entries = await Promise.all(
      names.split("\n").map((name) =>
        api.db
          .stats(name)
          .then((s) => [name, s] as const)
          .catch(() => null),
      ),
    );
    return new Map(entries.filter((e): e is readonly [string, DatabaseStatsResponse] => e !== null));
  }, [names]);

  const rows: Row[] = useMemo(() => {
    if (databases.state !== "ready") return [];
    const byName = stats.state === "ready" ? stats.data : new Map<string, DatabaseStatsResponse>();
    return databases.data.map((d) => ({ ...d, stats: byName.get(d.name) }));
  }, [databases, stats]);

  const refused = databases.state === "refused" ? databases.refused : undefined;
  const open = (name: string) => navigate(`/databases/${encodeURIComponent(name)}`);

  const columns: DataColumn<Row>[] = [
    {
      key: "name",
      header: "Name",
      // The row opens the database (click or Enter), so the name is text, not a second control
      render: (d) => <span className="st-link-text">{d.name}</span>,
    },
    { key: "type", header: "Type", render: (d) => <Chip>{modelName(d.database_type)}</Chip> },
    { key: "nodes", header: "Nodes", align: "end", render: (d) => fmt(d.stats?.node_count ?? d.node_count) },
    { key: "edges", header: "Edges", align: "end", render: (d) => fmt(d.stats?.edge_count ?? d.edge_count) },
    { key: "storage", header: "Storage", render: (d) => (d.persistent ? "Persistent" : "In memory") },
    {
      key: "memory",
      header: "Memory",
      align: "end",
      render: (d) =>
        d.stats ? (
          bytesText(d.stats.memory_bytes)
        ) : (
          <span className="st-muted">{stats.state === "loading" ? "…" : "unknown"}</span>
        ),
    },
    {
      key: "disk",
      header: "Disk",
      align: "end",
      render: (d) =>
        d.stats?.disk_bytes != null ? (
          bytesText(d.stats.disk_bytes)
        ) : (
          <span className="st-muted" title="In-memory databases keep nothing on disk">
            none
          </span>
        ),
    },
  ];

  let meta = host;
  if (databases.state === "ready") {
    meta = databases.data.length ? `${count(databases.data.length, "database")} on ${host}` : `No databases on ${host}`;
  }

  const totals = rows.reduce(
    (t, d) => ({
      nodes: t.nodes + (d.stats?.node_count ?? d.node_count),
      edges: t.edges + (d.stats?.edge_count ?? d.edge_count),
      memory: d.stats ? (t.memory ?? 0) + d.stats.memory_bytes : t.memory,
    }),
    { nodes: 0, edges: 0, memory: undefined as number | undefined },
  );

  return (
    <Page
      title="Databases"
      meta={meta}
      actions={
        <Button
          variant="primary"
          disabled={!!refused}
          title={refused ? "The server refused the database list, so a new database can't be made from here." : undefined}
          onClick={() => setCreating(true)}
        >
          New database
        </Button>
      }
    >
      {databases.state === "ready" ? (
        <div className="st-stats-wide">
          <Stats
            items={[
              ["Databases", fmt(rows.length)],
              ["Nodes", fmt(totals.nodes)],
              ["Edges", fmt(totals.edges)],
              bytesStat("Memory", totals.memory),
              ["Sessions", health ? fmt(health.active_sessions) : "…"],
              ["Server", health?.version ?? "…", health ? (health.persistent ? "persistent" : "in memory") : undefined],
            ]}
          />
        </div>
      ) : null}
      {databases.state === "failed" ? (
        <p className="st-error" role="alert">
          The database list did not load: {databases.message}
        </p>
      ) : (
        <DataTable
          label="Databases"
          columns={columns}
          rows={rows}
          rowKey={(d) => d.name}
          onRowClick={(d) => open(d.name)}
          empty={databases.state === "loading" ? undefined : "No databases yet. New database creates one."}
          refused={refused}
        />
      )}
      <NewDatabaseDialog
        open={creating}
        onClose={() => setCreating(false)}
        onCreated={(name) => {
          setCreating(false);
          server.refreshDatabases();
          open(name);
        }}
      />
    </Page>
  );
}
