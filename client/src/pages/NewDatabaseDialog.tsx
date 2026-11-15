import { useEffect, useState, type FormEvent } from "react";
import { Button, Dialog } from "@grafeo-db/ui";
import { api, GrafeoApiError } from "../api/client";
import type { DatabaseType, SectionName, StorageMode, SystemResources, TierOverride } from "../types/api";
import { bytesText } from "../shell/format";

const TYPES: { id: DatabaseType; name: string; gloss: string }[] = [
  { id: "Lpg", name: "LPG", gloss: "labeled property graph" },
  { id: "Rdf", name: "RDF", gloss: "triple store, queried with SPARQL" },
  { id: "OwlSchema", name: "OWL schema", gloss: "RDF with an OWL ontology" },
  { id: "RdfsSchema", name: "RDFS schema", gloss: "RDF with an RDFS schema" },
  { id: "JsonSchema", name: "JSON Schema", gloss: "LPG with JSON Schema constraints" },
];
const SCHEMA_TYPES = new Set<DatabaseType>(["OwlSchema", "RdfsSchema", "JsonSchema"]);
const SECTIONS: SectionName[] = [
  "LpgStore", "RdfStore", "CompactStore", "VectorStore", "TextIndex", "RdfRing", "PropertyIndex", "Catalog",
];
/** Sections the engine cannot memory-map, so keeping them on disk is not offered. */
const NON_SPILLABLE = new Set<SectionName>(["Catalog", "LpgStore", "RdfStore"]);
const DURABILITY = [
  { id: "sync", name: "sync: every commit is on disk before it returns" },
  { id: "batch", name: "batch: commits are written in groups" },
  { id: "adaptive", name: "adaptive: batch under load, sync when quiet" },
  { id: "nosync", name: "nosync: the operating system decides" },
] as const;
type Durability = (typeof DURABILITY)[number]["id"];

const MB = 1024 * 1024;
const MIN_MEMORY_MB = 64;

/** A file's bytes as base64, for the JSON request body. */
async function base64(file: File): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(binary);
}

interface Props {
  open: boolean;
  onClose(): void;
  onCreated(name: string): void;
}

/**
 * New database: the name is the one field a database needs; every other option has the server's default and sits
 * under "More options".
 */
export default function NewDatabaseDialog(props: Props) {
  return props.open ? <OpenNewDatabaseDialog {...props} /> : null;
}

/** The open dialog: mounted only while open, so each opening starts from the server's defaults. */
function OpenNewDatabaseDialog({ onClose, onCreated }: Props) {
  const [resources, setResources] = useState<SystemResources | null>(null);
  const [name, setName] = useState("");
  const [type, setType] = useState<DatabaseType>("Lpg");
  const [schemaFile, setSchemaFile] = useState<File | null>(null);
  const [storage, setStorage] = useState<StorageMode>("InMemory");
  const [memoryMb, setMemoryMb] = useState("");
  const [wal, setWal] = useState(false);
  const [durability, setDurability] = useState<Durability>("batch");
  const [backwardEdges, setBackwardEdges] = useState(true);
  const [tiers, setTiers] = useState<Partial<Record<SectionName, TierOverride>>>({});
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  // The server's defaults fill the form once they arrive.
  useEffect(() => {
    let cancelled = false;
    api.system
      .resources()
      .then((r) => {
        if (cancelled) return;
        setResources(r);
        setStorage(r.persistent_available ? (r.defaults.storage_mode as StorageMode) : "InMemory");
        setMemoryMb(String(Math.round(r.defaults.memory_limit_bytes / MB)));
        setWal(r.defaults.wal_enabled);
        setDurability((r.defaults.wal_durability as Durability) ?? "batch");
        setBackwardEdges(r.defaults.backward_edges);
      })
      .catch(() => {
        // Without the server's limits the form keeps its own defaults; the server still checks the request.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const types = resources ? TYPES.filter((t) => resources.available_types.includes(t.id)) : TYPES.slice(0, 2);
  const needsSchema = SCHEMA_TYPES.has(type);
  const memory = Number(memoryMb);
  const memoryValid = memoryMb.trim() === "" || (Number.isInteger(memory) && memory >= MIN_MEMORY_MB);
  const canCreate = name.trim() !== "" && (!needsSchema || schemaFile !== null) && memoryValid && !submitting;

  const submit = async (e?: FormEvent) => {
    e?.preventDefault();
    if (!canCreate) return;
    setError(null);
    setSubmitting(true);
    try {
      const trimmed = name.trim();
      await api.db.create({
        name: trimmed,
        database_type: type,
        storage_mode: storage,
        options: {
          memory_limit_bytes: memoryMb.trim() === "" ? undefined : memory * MB,
          wal_enabled: wal,
          wal_durability: wal ? durability : undefined,
          backward_edges: backwardEdges,
          section_tiers: storage === "Persistent" && Object.keys(tiers).length > 0 ? tiers : undefined,
        },
        schema_file: needsSchema && schemaFile ? await base64(schemaFile) : undefined,
        schema_filename: needsSchema && schemaFile ? schemaFile.name : undefined,
      });
      onCreated(trimmed);
    } catch (err) {
      setError(err instanceof GrafeoApiError ? err.detail : err instanceof Error ? err.message : String(err));
    } finally {
      setSubmitting(false);
    }
  };

  const setTier = (section: SectionName, tier: TierOverride) =>
    setTiers((prev) => {
      const next = { ...prev };
      if (tier === "auto") delete next[section];
      else next[section] = tier;
      return next;
    });

  return (
    <Dialog
      open
      onClose={onClose}
      title="New database"
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={!canCreate} onClick={() => void submit()}>
            {submitting ? "Creating…" : "Create database"}
          </Button>
        </>
      }
    >
      <form className="st-fields" onSubmit={(e) => void submit(e)}>
        <div className="gu-field">
          <label htmlFor="st-db-name">Name</label>
          <input
            id="st-db-name"
            className="gu-input"
            placeholder="my-graph"
            value={name}
            autoFocus
            onChange={(e) => setName(e.target.value)}
          />
        </div>

        <details className="st-more">
          <summary>More options</summary>

          <fieldset className="st-group">
            <legend>Graph model</legend>
            <div className="st-fields">
              <div className="gu-field">
                <label htmlFor="st-db-type">Type</label>
                <select
                  id="st-db-type"
                  className="gu-input"
                  value={type}
                  onChange={(e) => setType(e.target.value as DatabaseType)}
                >
                  {types.map((t) => (
                    <option key={t.id} value={t.id}>
                      {t.name}: {t.gloss}
                    </option>
                  ))}
                </select>
              </div>
              {needsSchema ? (
                <div className="gu-field">
                  <label htmlFor="st-db-schema">Schema file</label>
                  <input
                    id="st-db-schema"
                    className="gu-input"
                    type="file"
                    accept={type === "JsonSchema" ? ".json" : ".ttl,.owl,.rdf,.xml,.nt,.n3,.jsonld"}
                    onChange={(e) => setSchemaFile(e.target.files?.[0] ?? null)}
                  />
                  <span className="st-hint">This type needs its schema. The database is created with it loaded.</span>
                </div>
              ) : null}
            </div>
          </fieldset>

          <fieldset className="st-group">
            <legend>Storage</legend>
            <div className="st-fields">
              <div className="gu-field">
                <label htmlFor="st-db-storage">Mode</label>
                <select
                  id="st-db-storage"
                  className="gu-input"
                  value={storage}
                  disabled={resources !== null && !resources.persistent_available}
                  onChange={(e) => {
                    const next = e.target.value as StorageMode;
                    setStorage(next);
                    // A persistent database starts with its write-ahead log on.
                    if (next === "Persistent") setWal(true);
                  }}
                >
                  <option value="InMemory">In memory</option>
                  <option value="Persistent">Persistent</option>
                </select>
                {resources !== null && !resources.persistent_available ? (
                  <span className="st-hint">This server runs without a data directory, so databases live in memory.</span>
                ) : null}
              </div>
              <div className="gu-field">
                <label htmlFor="st-db-memory">Memory limit (MB)</label>
                <input
                  id="st-db-memory"
                  className="gu-input st-epoch"
                  type="number"
                  min={MIN_MEMORY_MB}
                  step={64}
                  value={memoryMb}
                  aria-invalid={!memoryValid || undefined}
                  onChange={(e) => setMemoryMb(e.target.value)}
                />
                <span className="st-hint">
                  At least {MIN_MEMORY_MB} MB.
                  {resources ? ` ${bytesText(resources.available_memory_bytes)} available on the server.` : null}
                  {storage === "Persistent" ? " Past the limit, sections spill to disk." : null}
                </span>
              </div>
              {storage === "Persistent" ? (
                <div className="gu-field">
                  <span className="st-field-label">Section tiers</span>
                  <span className="st-hint">Where each section lives; Automatic follows the memory limit.</span>
                  <div className="st-fields">
                    {SECTIONS.map((section) => (
                      <div className="st-field-row" key={section}>
                        <label htmlFor={`st-tier-${section}`} className="st-mono st-tier-label">
                          {section}
                        </label>
                        <select
                          id={`st-tier-${section}`}
                          className="gu-input"
                          value={tiers[section] ?? "auto"}
                          onChange={(e) => setTier(section, e.target.value as TierOverride)}
                        >
                          <option value="auto">Automatic</option>
                          <option value="force_ram">Keep in RAM</option>
                          {NON_SPILLABLE.has(section) ? null : <option value="force_disk">Keep on disk</option>}
                        </select>
                      </div>
                    ))}
                  </div>
                </div>
              ) : null}
            </div>
          </fieldset>

          <fieldset className="st-group">
            <legend>Durability</legend>
            <div className="st-fields">
              <label className="st-inline">
                <input type="checkbox" checked={wal} onChange={(e) => setWal(e.target.checked)} />
                Write-ahead log
              </label>
              {wal ? (
                <div className="gu-field">
                  <label htmlFor="st-db-durability">Commit durability</label>
                  <select
                    id="st-db-durability"
                    className="gu-input"
                    value={durability}
                    onChange={(e) => setDurability(e.target.value as Durability)}
                  >
                    {DURABILITY.map((d) => (
                      <option key={d.id} value={d.id}>
                        {d.name}
                      </option>
                    ))}
                  </select>
                </div>
              ) : null}
            </div>
          </fieldset>

          <fieldset className="st-group">
            <legend>Traversal</legend>
            <div className="st-fields">
              <label className="st-inline">
                <input type="checkbox" checked={backwardEdges} onChange={(e) => setBackwardEdges(e.target.checked)} />
                Backward edges
              </label>
              <span className="st-hint">Incoming relationships are found as fast as outgoing ones, for more memory.</span>
            </div>
          </fieldset>
        </details>

        {error ? (
          <p className="st-error" role="alert">
            {error}
          </p>
        ) : null}
        {/* Enter in a field submits */}
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
}
