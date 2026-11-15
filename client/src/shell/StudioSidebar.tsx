import { useMemo } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { SchemaList, Sidebar, SidebarItem, SidebarSection, StatusItem, type LanguageId } from "@grafeo-db/ui";
import { useServer } from "../context/ServerContext";
import { useWorkbench } from "../context/WorkbenchContext";
import { fmt, formatTimeAgo, formatUptime } from "./format";

/** Queries to start from; the names are the button names. */
const EXAMPLES: { name: string; query: string; language?: LanguageId }[] = [
  { name: "All nodes", query: "MATCH (n) RETURN n LIMIT 25", language: "gql" },
  { name: "All relationships", query: "MATCH (a)-[r]->(b) RETURN a, r, b LIMIT 25", language: "gql" },
  { name: "Count nodes", query: "MATCH (n) RETURN count(n) AS nodes", language: "gql" },
  { name: "Node labels", query: "MATCH (n) RETURN DISTINCT labels(n) AS labels", language: "gql" },
  {
    name: "Insert two people",
    query: "INSERT (:Person {name: 'Richard Hendricks'})-[:KNOWS]->(:Person {name: 'Jared Dunn'})",
    language: "gql",
  },
  {
    name: "Find by name",
    query: "MATCH (p:Person {name: 'Richard Hendricks'})-[r]-(other) RETURN p, r, other",
    language: "gql",
  },
];

const RDF_EXAMPLES: { name: string; query: string; language?: LanguageId }[] = [
  { name: "All triples", query: "SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 25", language: "sparql" },
  { name: "Count triples", query: "SELECT (COUNT(*) AS ?triples) WHERE { ?s ?p ?o }", language: "sparql" },
  {
    name: "Insert a triple",
    query: 'INSERT DATA { <http://example.org/richard> <http://xmlns.com/foaf/0.1/name> "Richard Hendricks" }',
    language: "sparql",
  },
];

const MAX_HISTORY_SHOWN = 10;

function ServerRows() {
  const { health, reachable } = useServer();
  return (
    <dl className="st-kv">
      <div>
        <dt>Status</dt>
        <dd>
          {reachable === false ? (
            <StatusItem tone="error">Unreachable</StatusItem>
          ) : health ? (
            <StatusItem tone="ok">{health.read_only ? "Connected, read-only" : "Connected"}</StatusItem>
          ) : (
            <StatusItem>Connecting…</StatusItem>
          )}
        </dd>
      </div>
      <div>
        <dt>Engine</dt>
        <dd className="st-mono">{health?.engine_version ?? "…"}</dd>
      </div>
      <div>
        <dt>Storage</dt>
        <dd className="st-mono">{health ? (health.persistent ? "persistent" : "in memory") : "…"}</dd>
      </div>
      <div>
        <dt>Uptime</dt>
        <dd className="st-mono">{health ? formatUptime(health.uptime_seconds) : "…"}</dd>
      </div>
    </dl>
  );
}

export default function StudioSidebar() {
  const navigate = useNavigate();
  const { pathname } = useLocation();
  const server = useServer();
  const workbench = useWorkbench();
  const { databases, schema, activeDatabase, activeModel } = server;

  const schemaEntries = useMemo(() => {
    if (schema.state !== "ready") return { labels: [], edgeTypes: [] };
    return { labels: schema.data.labels, edgeTypes: schema.data.edge_types };
  }, [schema]);

  const examples = activeModel === "lpg" || activeModel === "json-schema" ? EXAMPLES : RDF_EXAMPLES;
  const go = (path: string) => {
    if (pathname !== path) navigate(path);
  };
  // A query from the sidebar opens in the workbench.
  const open = (query: string, language?: LanguageId) => {
    workbench.openQuery(query, language);
    go("/");
  };

  return (
    <Sidebar>
      <SidebarSection title="Studio">
        <SidebarItem label="Query" active={pathname === "/"} onClick={() => go("/")} />
        <SidebarItem
          label="Databases"
          meta={databases.state === "ready" ? fmt(databases.data.length) : undefined}
          active={pathname.startsWith("/databases")}
          onClick={() => go("/databases")}
        />
        <SidebarItem
          label="Saved queries"
          meta={fmt(workbench.saved.length)}
          active={pathname === "/saved-queries"}
          onClick={() => go("/saved-queries")}
        />
        {server.tokens.state === "refused" || (server.tokens.state === "ready" && server.tokens.data !== null) ? (
          <SidebarItem
            label="API tokens"
            meta={server.tokens.state === "ready" && server.tokens.data ? fmt(server.tokens.data.length) : undefined}
            active={pathname === "/tokens"}
            onClick={() => go("/tokens")}
          />
        ) : null}
      </SidebarSection>

      <SidebarSection title="Server" plain>
        <ServerRows />
      </SidebarSection>

      <SidebarSection title="Databases" refused={databases.state === "refused" ? databases.refused : undefined}>
        {databases.state === "ready"
          ? databases.data.map((db) => (
              <SidebarItem
                key={db.name}
                label={db.name}
                meta={`${fmt(db.node_count)} n · ${fmt(db.edge_count)} e`}
                active={db.name === activeDatabase}
                onClick={() => server.selectDatabase(db.name)}
              />
            ))
          : null}
      </SidebarSection>
      {databases.state === "failed" ? <p className="st-note">The database list did not load.</p> : null}

      <SidebarSection title={`Schema · ${activeDatabase}`} plain>
        {schema.state === "failed" ? (
          <p className="st-note">The schema did not load.</p>
        ) : schema.state === "loading" ? null : (
          <SchemaList
            labels={schemaEntries.labels}
            edgeTypes={schemaEntries.edgeTypes}
            refused={schema.state === "refused" ? schema.refused : undefined}
          />
        )}
      </SidebarSection>

      {workbench.history.length > 0 ? (
        // A wrapper only for the mono look of queries; it adds no box (display: contents)
        <div className="st-history">
          <SidebarSection title="History">
            {workbench.history.slice(0, MAX_HISTORY_SHOWN).map((entry) => (
              <SidebarItem
                key={`${entry.timestamp}-${entry.query}`}
                label={entry.query}
                meta={formatTimeAgo(entry.timestamp)}
                onClick={() => open(entry.query, entry.language as LanguageId)}
              />
            ))}
          </SidebarSection>
        </div>
      ) : null}

      <SidebarSection title="Examples">
        {examples.map((example) => (
          <SidebarItem
            key={example.name}
            label={example.name}
            onClick={() => open(example.query, example.language)}
          />
        ))}
      </SidebarSection>
    </Sidebar>
  );
}
