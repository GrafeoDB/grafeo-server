import { useMemo } from "react";
import Graph from "graphology";
import type { QueryResponse } from "../types/api";

interface GraphNode {
  id: string;
  labels?: string[];
  label?: string;
  [key: string]: unknown;
}

interface GraphEdge {
  source?: string;
  target?: string;
  start?: string;
  end?: string;
  type?: string;
  label?: string;
  [key: string]: unknown;
}

const COLORS = [
  "#4fc3f7", "#66bb6a", "#ffa726", "#ef5350", "#ab47bc",
  "#26c6da", "#ff7043", "#9ccc65", "#5c6bc0", "#ec407a",
];

function labelColor(label: string): string {
  let hash = 0;
  for (let i = 0; i < label.length; i++) {
    hash = ((hash << 5) - hash + label.charCodeAt(i)) | 0;
  }
  return COLORS[Math.abs(hash) % COLORS.length];
}

function isNode(cell: unknown): cell is GraphNode {
  if (typeof cell !== "object" || cell === null) return false;
  const obj = cell as Record<string, unknown>;
  return "id" in obj && ("labels" in obj || "label" in obj);
}

function isEdge(cell: unknown): cell is GraphEdge {
  if (typeof cell !== "object" || cell === null) return false;
  const obj = cell as Record<string, unknown>;
  return (
    ("source" in obj && "target" in obj) ||
    ("start" in obj && "end" in obj)
  );
}

export interface GraphData {
  graph: Graph;
  nodeData: Map<string, Record<string, unknown>>;
}

/**
 * Detect if columns follow the (node, edge, node) triplet pattern.
 * The engine returns integer IDs for nodes and edges, e.g.:
 *   columns: ["a", "r", "b"], rows: [[4, 0, 5]]
 *
 * We look for groups of 3 columns where all values are integers,
 * treating columns 0,2 as nodes and column 1 as an edge.
 *
 * Also handles single-column or multi-column node-only results
 * like: columns: ["n"], rows: [[0], [1], [2]]
 */
function buildGraphFromScalars(result: QueryResponse): GraphData | null {
  const { columns, rows } = result;
  if (rows.length === 0) return null;

  // Check which columns contain only integer (or null) values
  const intCols = columns.map((_, ci) =>
    rows.every((row) => {
      const v = row[ci];
      return v === null || v === undefined || (typeof v === "number" && Number.isInteger(v));
    }),
  );

  // All values must be integers for scalar graph detection
  if (!intCols.some(Boolean)) return null;

  const graph = new Graph({ multi: true, type: "directed" });
  const nodeData = new Map<string, Record<string, unknown>>();
  const addedNodes = new Set<string>();

  const ensureNode = (id: string, colName: string) => {
    if (addedNodes.has(id)) return;
    addedNodes.add(id);
    graph.addNode(id, {
      label: `${colName} ${id}`,
      size: 10,
      color: labelColor(colName),
      x: Math.random() * 100,
      y: Math.random() * 100,
    });
    nodeData.set(id, { id: Number(id), column: colName });
  };

  // Detect triplet patterns: (node, edge, node) groups of 3 integer columns
  const intColIndices = columns.map((_, i) => i).filter((i) => intCols[i]);

  if (intColIndices.length >= 3 && intColIndices.length % 3 === 0) {
    // Process in groups of 3: (source_col, edge_col, target_col)
    for (let g = 0; g < intColIndices.length; g += 3) {
      const srcIdx = intColIndices[g];
      const edgeIdx = intColIndices[g + 1];
      const tgtIdx = intColIndices[g + 2];
      const srcCol = columns[srcIdx];
      const tgtCol = columns[tgtIdx];
      const edgeCol = columns[edgeIdx];

      for (const row of rows) {
        const src = row[srcIdx];
        const tgt = row[tgtIdx];
        const edgeId = row[edgeIdx];
        if (src == null || tgt == null) continue;

        const srcId = String(src);
        const tgtId = String(tgt);
        ensureNode(srcId, srcCol);
        ensureNode(tgtId, tgtCol);

        // Update node data with any extra non-integer columns for this row
        for (let ci = 0; ci < columns.length; ci++) {
          if (intCols[ci]) continue;
          const val = row[ci];
          const col = columns[ci];
          // Try to associate extra columns with source node
          const existing = nodeData.get(srcId);
          if (existing) existing[col] = val;
        }

        const eid = `e${edgeId ?? `${srcId}-${tgtId}`}`;
        if (!graph.hasEdge(eid)) {
          graph.addEdgeWithKey(eid, srcId, tgtId, {
            label: edgeCol,
            size: 2,
            color: "#64748b",
          });
        }
      }
    }
  } else {
    // No triplet pattern — treat all integer columns as nodes
    for (const row of rows) {
      for (let ci = 0; ci < columns.length; ci++) {
        if (!intCols[ci]) continue;
        const val = row[ci];
        if (val == null) continue;
        const id = String(val);
        const colName = columns[ci];
        ensureNode(id, colName);

        // Attach any non-integer column values as properties
        for (let pi = 0; pi < columns.length; pi++) {
          if (intCols[pi] || pi === ci) continue;
          const propVal = row[pi];
          const existing = nodeData.get(id);
          if (existing) existing[columns[pi]] = propVal;
        }
      }
    }
  }

  if (graph.order === 0) return null;
  return { graph, nodeData };
}

export function useGraphData(result: QueryResponse | null): GraphData | null {
  return useMemo(() => {
    if (!result || result.rows.length === 0) return null;

    const graph = new Graph({ multi: true, type: "directed" });
    const nodes = new Map<string, GraphNode>();
    const edges: GraphEdge[] = [];

    // Strategy 1: Extract rich node/edge objects from cells
    for (const row of result.rows) {
      for (const cell of row) {
        if (isNode(cell)) {
          nodes.set(String(cell.id), cell);
        } else if (isEdge(cell)) {
          edges.push(cell);
        }
      }
    }

    if (nodes.size > 0) {
      const nodeData = new Map<string, Record<string, unknown>>();

      for (const [id, node] of nodes) {
        const primaryLabel =
          (node.labels && node.labels[0]) || node.label || "Node";
        const displayName =
          (node.name as string) || (node.title as string) || id;

        graph.addNode(id, {
          label: displayName,
          size: 10,
          color: labelColor(primaryLabel),
          x: Math.random() * 100,
          y: Math.random() * 100,
        });

        nodeData.set(id, node as Record<string, unknown>);
      }

      for (const edge of edges) {
        const source = String(edge.source ?? edge.start ?? "");
        const target = String(edge.target ?? edge.end ?? "");
        if (graph.hasNode(source) && graph.hasNode(target)) {
          graph.addEdge(source, target, {
            label: edge.type || edge.label || "",
            size: 2,
            color: "#64748b",
          });
        }
      }

      return { graph, nodeData };
    }

    // Strategy 2: Build graph from scalar integer IDs
    // (engine returns node/edge IDs as plain integers)
    return buildGraphFromScalars(result);
  }, [result]);
}
