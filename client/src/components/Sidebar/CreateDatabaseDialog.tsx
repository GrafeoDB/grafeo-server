import { useState, useEffect, useRef, useCallback } from "react";
import { api, GrafeoApiError } from "../../api/client";
import type {
  DatabaseType,
  StorageMode,
  SystemResources,
} from "../../types/api";
import styles from "./CreateDatabaseDialog.module.css";

interface CreateDatabaseDialogProps {
  open: boolean;
  onClose: () => void;
  onCreated: () => void;
}

const DB_TYPE_LABELS: Record<string, { label: string; desc: string }> = {
  Lpg: { label: "LPG", desc: "Labeled Property Graph" },
  Rdf: { label: "RDF", desc: "Triple store (SPARQL)" },
  OwlSchema: { label: "OWL Schema", desc: "RDF + OWL ontology" },
  RdfsSchema: { label: "RDFS Schema", desc: "RDF + RDFS schema" },
  JsonSchema: { label: "JSON Schema", desc: "LPG + constraints" },
};

const SCHEMA_TYPES = new Set<DatabaseType>([
  "OwlSchema",
  "RdfsSchema",
  "JsonSchema",
]);

function formatBytes(bytes: number): string {
  if (bytes >= 1024 * 1024 * 1024) {
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  }
  return `${Math.round(bytes / (1024 * 1024))} MB`;
}

export default function CreateDatabaseDialog({
  open,
  onClose,
  onCreated,
}: CreateDatabaseDialogProps) {
  const [resources, setResources] = useState<SystemResources | null>(null);
  const [name, setName] = useState("");
  const [dbType, setDbType] = useState<DatabaseType>("Lpg");
  const [storageMode, setStorageMode] = useState<StorageMode>("InMemory");
  const [memoryLimit, setMemoryLimit] = useState(512 * 1024 * 1024);
  const [walEnabled, setWalEnabled] = useState(false);
  const [walDurability, setWalDurability] = useState("batch");
  const [backwardEdges, setBackwardEdges] = useState(true);
  const [threads, setThreads] = useState(0); // 0 = auto
  const [schemaFile, setSchemaFile] = useState<File | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const fetchResources = useCallback(() => {
    api.system
      .resources()
      .then((res) => {
        setResources(res);
        setMemoryLimit(res.defaults.memory_limit_bytes);
        setThreads(res.defaults.threads);
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (open) {
      fetchResources();
      setName("");
      setDbType("Lpg");
      setStorageMode("InMemory");
      setWalEnabled(false);
      setWalDurability("batch");
      setBackwardEdges(true);
      setSchemaFile(null);
      setError(null);
    }
  }, [open, fetchResources]);

  // Auto-enable WAL when switching to persistent
  useEffect(() => {
    if (storageMode === "Persistent") {
      setWalEnabled(true);
    }
  }, [storageMode]);

  if (!open) return null;

  const memoryMin = 64 * 1024 * 1024;
  const memoryMax = resources?.available_memory_bytes ?? 4 * 1024 * 1024 * 1024;
  const memoryStep = 64 * 1024 * 1024;
  const needsSchema = SCHEMA_TYPES.has(dbType);

  const canSubmit =
    name.trim().length > 0 &&
    (!needsSchema || schemaFile !== null) &&
    !submitting;

  const handleFileSelect = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (file) setSchemaFile(file);
  };

  const handleSubmit = async () => {
    setError(null);
    setSubmitting(true);

    try {
      let schemaB64: string | undefined;
      let schemaFilename: string | undefined;

      if (schemaFile) {
        const buffer = await schemaFile.arrayBuffer();
        const bytes = new Uint8Array(buffer);
        let binary = "";
        for (const b of bytes) binary += String.fromCharCode(b);
        schemaB64 = btoa(binary);
        schemaFilename = schemaFile.name;
      }

      await api.db.create({
        name: name.trim(),
        database_type: dbType,
        storage_mode: storageMode,
        options: {
          memory_limit_bytes: memoryLimit,
          wal_enabled: walEnabled,
          wal_durability: walDurability as "sync" | "batch" | "adaptive" | "nosync",
          backward_edges: backwardEdges,
          threads: threads === 0 ? undefined : threads,
        },
        schema_file: schemaB64,
        schema_filename: schemaFilename,
      });

      onCreated();
      onClose();
    } catch (err) {
      if (err instanceof GrafeoApiError) {
        setError(err.detail);
      } else {
        setError(String(err));
      }
    } finally {
      setSubmitting(false);
    }
  };

  const handleOverlayClick = (e: React.MouseEvent) => {
    if (e.target === e.currentTarget) onClose();
  };

  return (
    <div className={styles.overlay} onClick={handleOverlayClick}>
      <div className={styles.dialog}>
        <div className={styles.title}>New Database</div>

        {/* Name */}
        <div className={styles.field}>
          <label className={styles.label}>Name</label>
          <input
            className={styles.input}
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="my-database"
            autoFocus
          />
        </div>

        {/* Database Type */}
        <div className={styles.field}>
          <label className={styles.label}>Database Type</label>
          <div className={styles.radioGroup}>
            {(resources?.available_types ?? ["Lpg"]).map((t) => {
              const info = DB_TYPE_LABELS[t];
              return (
                <button
                  key={t}
                  className={`${styles.radioOption} ${
                    dbType === t ? styles.selected : ""
                  }`}
                  onClick={() => setDbType(t as DatabaseType)}
                  title={info?.desc}
                >
                  {info?.label ?? t}
                </button>
              );
            })}
          </div>
        </div>

        {/* Schema File Upload */}
        {needsSchema && (
          <div className={styles.field}>
            <label className={styles.label}>Schema File</label>
            <input
              type="file"
              ref={fileInputRef}
              onChange={handleFileSelect}
              accept=".owl,.rdf,.rdfs,.json,.ttl"
              style={{ display: "none" }}
            />
            <div
              className={`${styles.fileUpload} ${
                schemaFile ? styles.hasFile : ""
              }`}
              onClick={() => fileInputRef.current?.click()}
            >
              {schemaFile ? (
                <span className={styles.fileName}>{schemaFile.name}</span>
              ) : (
                <span className={styles.fileHint}>
                  Click to select a schema file
                </span>
              )}
            </div>
          </div>
        )}

        {/* Storage Mode */}
        <div className={styles.field}>
          <label className={styles.label}>Storage</label>
          <div className={styles.toggle}>
            <div
              className={`${styles.toggleSwitch} ${
                storageMode === "Persistent" ? styles.on : ""
              }`}
              onClick={() => {
                if (
                  storageMode === "InMemory" &&
                  resources?.persistent_available
                ) {
                  setStorageMode("Persistent");
                } else {
                  setStorageMode("InMemory");
                }
              }}
            />
            <span className={styles.toggleLabel}>
              {storageMode === "Persistent" ? "Persistent" : "In-Memory"}
            </span>
            {!resources?.persistent_available && (
              <span className={styles.disabledNote}>
                (requires --data-dir)
              </span>
            )}
          </div>
        </div>

        {/* Memory Limit */}
        <div className={styles.field}>
          <label className={styles.label}>Memory Limit</label>
          <input
            type="range"
            className={styles.slider}
            min={memoryMin}
            max={memoryMax}
            step={memoryStep}
            value={memoryLimit}
            onChange={(e) => setMemoryLimit(Number(e.target.value))}
          />
          <div className={styles.sliderValue}>{formatBytes(memoryLimit)}</div>
        </div>

        {/* WAL */}
        <div className={styles.field}>
          <label className={styles.label}>Write-Ahead Log</label>
          <div className={styles.toggle}>
            <div
              className={`${styles.toggleSwitch} ${
                walEnabled ? styles.on : ""
              }`}
              onClick={() => setWalEnabled(!walEnabled)}
            />
            <span className={styles.toggleLabel}>
              {walEnabled ? "Enabled" : "Disabled"}
            </span>
          </div>
          {walEnabled && (
            <select
              className={styles.select}
              value={walDurability}
              onChange={(e) => setWalDurability(e.target.value)}
              style={{ marginTop: 6 }}
            >
              <option value="sync">Sync</option>
              <option value="batch">Batch (default)</option>
              <option value="adaptive">Adaptive</option>
              <option value="nosync">NoSync</option>
            </select>
          )}
        </div>

        {/* Backward Edges */}
        <div className={styles.field}>
          <label className={styles.label}>Backward Edges</label>
          <div className={styles.toggle}>
            <div
              className={`${styles.toggleSwitch} ${
                backwardEdges ? styles.on : ""
              }`}
              onClick={() => setBackwardEdges(!backwardEdges)}
            />
            <span className={styles.toggleLabel}>
              {backwardEdges ? "Enabled" : "Disabled"}
            </span>
          </div>
          <div className={styles.tooltip}>
            Disable to save ~50% adjacency memory. Reverse traversals use full
            scans.
          </div>
        </div>

        {/* Threads */}
        <div className={styles.field}>
          <label className={styles.label}>Threads</label>
          <select
            className={styles.select}
            value={threads}
            onChange={(e) => setThreads(Number(e.target.value))}
          >
            <option value={0}>
              Auto ({resources?.defaults.threads ?? "?"})
            </option>
            <option value={1}>1</option>
            <option value={2}>2</option>
            <option value={4}>4</option>
            <option value={8}>8</option>
          </select>
        </div>

        {/* Error */}
        {error && <div className={styles.error}>{error}</div>}

        {/* Actions */}
        <div className={styles.actions}>
          <button className={styles.cancelButton} onClick={onClose}>
            Cancel
          </button>
          <button
            className={styles.submitButton}
            onClick={handleSubmit}
            disabled={!canSubmit}
          >
            {submitting ? "Creating..." : "Create"}
          </button>
        </div>
      </div>
    </div>
  );
}
