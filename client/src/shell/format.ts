import { LANGUAGES, type LanguageId, type QueryResult } from "@grafeo-db/ui";

const number = new Intl.NumberFormat("en-US");

export function fmt(n: number): string {
  return number.format(n);
}

/** `1 node`, `13 nodes`. */
export function count(n: number, one: string, many = `${one}s`): string {
  return `${fmt(n)} ${n === 1 ? one : many}`;
}

export function formatBytes(bytes: number): [string, string] {
  if (bytes < 1024) return [String(bytes), "B"];
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return [value.toFixed(1), units[unit]];
}

export function bytesText(bytes: number): string {
  const [value, unit] = formatBytes(bytes);
  return `${value} ${unit}`;
}

export function formatUptime(seconds: number): string {
  const d = Math.floor(seconds / 86400);
  const h = Math.floor((seconds % 86400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  if (d > 0) return `${d} d ${h} h`;
  if (h > 0) return `${h} h ${m} m`;
  if (m > 0) return `${m} m`;
  return `${Math.floor(seconds)} s`;
}

/** `2026-11-14 08:40`, in the browser's time zone. */
export function formatDate(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

export function formatTimeAgo(timestamp: number): string {
  const diff = Math.floor((Date.now() - timestamp) / 1000);
  if (diff < 60) return "now";
  if (diff < 3600) return `${Math.floor(diff / 60)} m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)} h`;
  return `${Math.floor(diff / 86400)} d`;
}

export function languageName(id: LanguageId | string): string {
  return LANGUAGES.find((l) => l.id === id)?.name ?? id;
}

/** The graph model as the status bar and chips name it. */
export function modelName(databaseType: string): string {
  switch (databaseType.toLowerCase()) {
    case "rdf":
      return "RDF";
    case "owl-schema":
      return "OWL";
    case "rdfs-schema":
      return "RDFS";
    case "json-schema":
      return "JSON Schema";
    default:
      return "LPG";
  }
}

/** What the status bar says about a result: the graph's size, or the row count, with the time. */
export function resultLine(result: QueryResult): string {
  const size =
    result.nodes.length > 0
      ? `${count(result.nodes.length, "node")} · ${count(result.edges.length, "edge")}`
      : count(result.rows.length, "row");
  return result.timeMs === undefined ? size : `${size} · ${formatMs(result.timeMs)}`;
}

/** The results panel's own format, so the two never disagree: `1 ms`, `1,234 ms`. */
function formatMs(ms: number): string {
  return `${Math.round(ms).toLocaleString("en")} ms`;
}
