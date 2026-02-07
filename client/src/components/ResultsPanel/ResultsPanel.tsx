import { useState, useCallback } from "react";
import type { QueryResponse } from "../../types/api";
import { useGraphData } from "../../hooks/useGraphData";
import TableView from "./TableView";
import GraphView from "./GraphView";
import NodeDetailPanel from "./NodeDetailPanel";
import styles from "./ResultsPanel.module.css";

type ViewMode = "table" | "graph";

interface ResultsPanelProps {
  result: QueryResponse | null;
  viewMode: ViewMode;
  onViewModeChange: (mode: ViewMode) => void;
}

function escapeCSV(val: unknown): string {
  const s =
    val === null || val === undefined
      ? ""
      : typeof val === "object"
        ? JSON.stringify(val)
        : String(val);
  return s.includes(",") || s.includes('"') || s.includes("\n")
    ? `"${s.replace(/"/g, '""')}"`
    : s;
}

export default function ResultsPanel({
  result,
  viewMode,
  onViewModeChange,
}: ResultsPanelProps) {
  const [copyFeedback, setCopyFeedback] = useState<string | null>(null);
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);
  const graphData = useGraphData(result);

  const handleNodeSelect = useCallback((nodeId: string | null) => {
    setSelectedNodeId(nodeId);
  }, []);

  const showFeedback = (msg: string) => {
    setCopyFeedback(msg);
    setTimeout(() => setCopyFeedback(null), 2000);
  };

  const copyJSON = async () => {
    if (!result) return;
    const json = JSON.stringify(
      { columns: result.columns, rows: result.rows },
      null,
      2,
    );
    await navigator.clipboard.writeText(json);
    showFeedback("JSON copied!");
  };

  const copyCSV = async () => {
    if (!result) return;
    const header = result.columns.map(escapeCSV).join(",");
    const rows = result.rows.map((row) => row.map(escapeCSV).join(","));
    await navigator.clipboard.writeText([header, ...rows].join("\n"));
    showFeedback("CSV copied!");
  };

  return (
    <div className={styles.container}>
      <div className={styles.toggleBar}>
        <button
          className={`${styles.toggleButton} ${viewMode === "table" ? styles.active : ""}`}
          onClick={() => onViewModeChange("table")}
        >
          Table
        </button>
        <button
          className={`${styles.toggleButton} ${viewMode === "graph" ? styles.active : ""}`}
          onClick={() => onViewModeChange("graph")}
        >
          Graph
        </button>
        {result && (
          <>
            <button
              className={styles.exportButton}
              onClick={copyJSON}
              title="Copy as JSON"
            >
              Copy JSON
            </button>
            <button
              className={styles.exportButton}
              onClick={copyCSV}
              title="Copy as CSV"
            >
              Copy CSV
            </button>
          </>
        )}
        {copyFeedback && (
          <span className={styles.copyToast}>{copyFeedback}</span>
        )}
        {result && (
          <span className={styles.rowCount}>
            {result.rows.length} row{result.rows.length !== 1 ? "s" : ""}
          </span>
        )}
      </div>
      <div className={styles.content}>
        {!result ? (
          <div className={styles.empty}>Run a query to see results</div>
        ) : viewMode === "table" ? (
          <TableView columns={result.columns} rows={result.rows} />
        ) : (
          <div className={styles.graphLayout}>
            <GraphView
              result={result}
              onNodeSelect={handleNodeSelect}
              selectedNodeId={selectedNodeId}
            />
            <NodeDetailPanel
              node={
                selectedNodeId && graphData
                  ? (graphData.nodeData.get(selectedNodeId) ?? null)
                  : null
              }
            />
          </div>
        )}
      </div>
    </div>
  );
}
