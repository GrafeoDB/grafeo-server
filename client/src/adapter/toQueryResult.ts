import type { QueryResult, ResultEdge, ResultNode } from "@grafeo-db/ui";
import type { QueryResponse } from "../types/api";

/**
 * The server's answer as the package's one result shape. The server sends
 * columns and rows only; the graph is the nodes and edges inside the cells, in
 * every query language:
 *
 * - a node is `{_id, _labels, ...properties}`;
 * - an edge is `{_id, _type, _source, _target, ...properties}`;
 * - a path is `{nodes: [...], edges: [...]}`, and a list may hold any of these.
 *
 * Row cells that are a node or an edge are replaced by the same object the
 * graph gets, so the table draws them as a node or an edge.
 */
export function toQueryResult(response: QueryResponse): QueryResult {
  const nodes = new Map<string, ResultNode>();
  const edges = new Map<string, ResultEdge>();

  const visit = (value: unknown, depth: number) => {
    if (depth > MAX_DEPTH) return;
    if (Array.isArray(value)) {
      for (const item of value) visit(item, depth + 1);
    } else if (isNodeCell(value)) {
      const node = toNode(value);
      if (!nodes.has(node.id)) nodes.set(node.id, node);
    } else if (isEdgeCell(value)) {
      const edge = toEdge(value);
      if (!edges.has(edge.id)) edges.set(edge.id, edge);
    } else if (isPathCell(value)) {
      visit(value.nodes, depth + 1);
      visit(value.edges, depth + 1);
    }
  };
  for (const row of response.rows) for (const cell of row) visit(cell, 0);

  const rows = response.rows.map((row) =>
    row.map((cell) => {
      if (isNodeCell(cell)) return nodes.get(nodeId(cell)) ?? cell;
      if (isEdgeCell(cell)) return edges.get(edgeId(cell)) ?? cell;
      return cell;
    }),
  );

  return {
    columns: response.columns,
    rows,
    nodes: [...nodes.values()],
    // An edge is drawn only between nodes the result holds.
    edges: [...edges.values()].filter((e) => nodes.has(e.source) && nodes.has(e.target)),
    timeMs: response.execution_time_ms,
  };
}

/** The answer to `EXPLAIN <query>`: one `plan` column whose row holds the plan text. */
export function toPlan(response: QueryResponse): string | undefined {
  const index = response.columns.indexOf("plan");
  const cell = index >= 0 ? response.rows[0]?.[index] : undefined;
  return typeof cell === "string" ? cell : undefined;
}

/** Nested lists deeper than this are data, not graph. */
const MAX_DEPTH = 4;

type Cell = Record<string, unknown>;

function isObject(value: unknown): value is Cell {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isNodeCell(value: unknown): value is Cell {
  return isObject(value) && "_id" in value && Array.isArray(value._labels);
}

function isEdgeCell(value: unknown): value is Cell {
  return isObject(value) && "_id" in value && "_source" in value && "_target" in value;
}

function isPathCell(value: unknown): value is { nodes: unknown; edges: unknown } {
  return isObject(value) && Array.isArray(value.nodes) && Array.isArray(value.edges);
}

function nodeId(cell: Cell): string {
  return String(cell._id);
}

/** Edges have their own id space; the prefix keeps it apart from node ids. */
function edgeId(cell: Cell): string {
  return `e${String(cell._id)}`;
}

/** The user's properties: everything but the server's `_` fields. */
function propertiesOf(cell: Cell): Record<string, unknown> {
  const properties: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(cell)) {
    if (!key.startsWith("_")) properties[key] = value;
  }
  return properties;
}

function toNode(cell: Cell): ResultNode {
  const labels = (cell._labels as unknown[]).map(String);
  return { id: nodeId(cell), labels, properties: propertiesOf(cell) };
}

function toEdge(cell: Cell): ResultEdge {
  return {
    id: edgeId(cell),
    type: typeof cell._type === "string" ? cell._type : "",
    source: String(cell._source),
    target: String(cell._target),
    properties: propertiesOf(cell),
  };
}
