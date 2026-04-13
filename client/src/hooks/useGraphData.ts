import { useMemo } from "react";
import Graph from "graphology";
import type { QueryResponse } from "../types/api";

/**
 * Shape returned by grafeo's Cypher/GQL adapter for node-valued columns.
 * The server prefixes internal fields with underscores and exposes user
 * properties at the top level alongside them.
 */
interface GrafeoNode {
  _id: number | string;
  _labels?: string[];
  /** Legacy fallback for shapes that use `id`/`labels`/`label`. */
  id?: number | string;
  labels?: string[];
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

function isNode(cell: unknown): cell is GrafeoNode {
  if (typeof cell !== "object" || cell === null) return false;
  const obj = cell as Record<string, unknown>;
  // Grafeo's Cypher/GQL adapter: {_id, _labels, ...props}
  if ("_id" in obj && "_labels" in obj) return true;
  // Legacy / third-party shape fallback.
  return "id" in obj && ("labels" in obj || "label" in obj);
}

function nodeId(node: GrafeoNode): string {
  const raw = node._id ?? node.id;
  return String(raw);
}

function nodeLabels(node: GrafeoNode): string[] {
  if (Array.isArray(node._labels)) return node._labels;
  if (Array.isArray(node.labels)) return node.labels;
  if (typeof node.label === "string") return [node.label];
  return [];
}

/**
 * Pick a human-readable display name for a node. Prefers conventional
 * property names, then any user-supplied `id` property (e.g. sensor_1),
 * then falls back to the internal node id.
 */
function nodeDisplayName(node: GrafeoNode, fallback: string): string {
  const candidates: Array<unknown> = [
    node.name,
    node.title,
    node.label,
    // `id` here is the user-supplied property, distinct from `_id` which
    // is the internal numeric id we already use as the graph key.
    "_id" in node ? node.id : undefined,
  ];
  for (const v of candidates) {
    if (typeof v === "string" && v.length > 0) return v;
    if (typeof v === "number") return String(v);
  }
  return fallback;
}

export interface GraphData {
  graph: Graph;
  nodeData: Map<string, Record<string, unknown>>;
}

/**
 * Detect triplet patterns `(int, int, int)` in the column layout where
 * the engine returns bare integer IDs for nodes and edges, e.g.:
 *   columns: ["a", "r", "b"], rows: [[4, 0, 5]]
 *
 * Also handles single-column or multi-column node-only results like
 *   columns: ["n"], rows: [[0], [1], [2]]
 */
function buildGraphFromScalars(result: QueryResponse): GraphData | null {
  const { columns, rows } = result;
  if (rows.length === 0) return null;

  const intCols = columns.map((_, ci) =>
    rows.every((row) => {
      const v = row[ci];
      return v === null || v === undefined || (typeof v === "number" && Number.isInteger(v));
    }),
  );

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

  const intColIndices = columns.map((_, i) => i).filter((i) => intCols[i]);

  if (intColIndices.length >= 3 && intColIndices.length % 3 === 0) {
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

        for (let ci = 0; ci < columns.length; ci++) {
          if (intCols[ci]) continue;
          const val = row[ci];
          const col = columns[ci];
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
    for (const row of rows) {
      for (let ci = 0; ci < columns.length; ci++) {
        if (!intCols[ci]) continue;
        const val = row[ci];
        if (val == null) continue;
        const id = String(val);
        const colName = columns[ci];
        ensureNode(id, colName);

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
    const nodes = new Map<string, GrafeoNode>();

    // Strategy 1: pull node objects out of every cell.
    for (const row of result.rows) {
      for (const cell of row) {
        if (isNode(cell)) {
          nodes.set(nodeId(cell), cell);
        }
      }
    }

    if (nodes.size > 0) {
      const nodeData = new Map<string, Record<string, unknown>>();

      for (const [id, node] of nodes) {
        const labels = nodeLabels(node);
        const primaryLabel = labels[0] ?? "Node";
        const displayName = nodeDisplayName(node, id);

        graph.addNode(id, {
          label: displayName,
          size: 10,
          color: labelColor(primaryLabel),
          x: Math.random() * 100,
          y: Math.random() * 100,
        });

        nodeData.set(id, node as Record<string, unknown>);
      }

      // Strategy 1b: infer edges from the column layout.
      //
      // Grafeo's Cypher/GQL adapter returns relationships as bare
      // integer ids (e.g. `[nodeA, 0, nodeB]` for a
      // `MATCH (a)-[r]->(b) RETURN a, r, b` query). There's no object
      // to inspect for source/target — we have to reconstruct the
      // path from the column types.
      //
      // Classify each column by what it holds across rows (ignoring
      // nulls), then if the columns resolve to K node columns and
      // (K-1) integer columns we treat each row as a path of K nodes
      // with the integers as the edges between them. Column order in
      // the RETURN clause defines the path order, so `RETURN n, m, r`
      // and `RETURN n, r, m` both draw an edge from n to m regardless
      // of where the integer column lands.
      const { columns, rows } = result;
      type ColKind = "node" | "integer" | "other";

      // Columns named like a function call — count(*), sum(x), id(n),
      // length(path) — are aggregations or projections, not edge ids.
      // Excluding them here stops the path heuristic from false-firing
      // on queries like RETURN n, m, count(*), where the column shape
      // (2 nodes + 1 integer) otherwise looks exactly like an edge.
      const looksLikeAggregation = (colName: string): boolean =>
        /\w\s*\(/.test(colName);

      const kinds: ColKind[] = columns.map((colName, ci) => {
        if (looksLikeAggregation(colName)) return "other";
        let sawNode = false;
        let sawInt = false;
        let sawOther = false;
        for (const row of rows) {
          const v = row[ci];
          if (v == null) continue;
          if (isNode(v)) {
            sawNode = true;
          } else if (typeof v === "number" && Number.isInteger(v)) {
            sawInt = true;
          } else {
            sawOther = true;
          }
        }
        // Homogeneous columns only — mixed-type columns fall through
        // as "other" and don't participate in edge inference.
        if (sawNode && !sawInt && !sawOther) return "node";
        if (sawInt && !sawNode && !sawOther) return "integer";
        return "other";
      });

      const nodeColIndices = kinds
        .map((k, i) => (k === "node" ? i : -1))
        .filter((i) => i >= 0);
      const intColIndices = kinds
        .map((k, i) => (k === "integer" ? i : -1))
        .filter((i) => i >= 0);

      // Only wire edges when the shape is a clean path: K node columns
      // and exactly (K-1) integer columns, K >= 2. This catches the
      // common MATCH (a)-[r]->(b) case and the multi-hop
      // MATCH (a)-[r1]->(b)-[r2]->(c) case without firing on
      // aggregations like RETURN n, count(n) (1 node + 1 int).
      const looksLikePath =
        nodeColIndices.length >= 2 &&
        intColIndices.length === nodeColIndices.length - 1;

      if (looksLikePath) {
        for (const row of rows) {
          for (let k = 0; k < intColIndices.length; k++) {
            const srcCell = row[nodeColIndices[k]];
            const tgtCell = row[nodeColIndices[k + 1]];
            const edgeIdCell = row[intColIndices[k]];
            if (
              !isNode(srcCell) ||
              !isNode(tgtCell) ||
              typeof edgeIdCell !== "number"
            ) {
              continue;
            }

            const src = nodeId(srcCell);
            const tgt = nodeId(tgtCell);
            if (!graph.hasNode(src) || !graph.hasNode(tgt)) continue;

            // Key by (edge_id, src, tgt) so parallel edges and
            // self-loops don't collide in the multigraph.
            const key = `e${edgeIdCell}-${src}-${tgt}`;
            if (!graph.hasEdge(key)) {
              graph.addEdgeWithKey(key, src, tgt, {
                label: String(edgeIdCell),
                size: 2,
                color: "#64748b",
              });
            }
          }
        }
      }

      return { graph, nodeData };
    }

    // Strategy 2: bare integer columns (legacy engine output).
    return buildGraphFromScalars(result);
  }, [result]);
}
