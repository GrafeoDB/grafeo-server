import styles from "./ResultsPanel.module.css";

interface TableViewProps {
  columns: string[];
  rows: unknown[][];
}

function formatCell(value: unknown): string {
  if (value === null || value === undefined) return "null";
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}

export default function TableView({ columns, rows }: TableViewProps) {
  if (columns.length === 0) {
    return <div className={styles.empty}>Query returned no columns</div>;
  }

  return (
    <div style={{ overflow: "auto", height: "100%" }}>
      <table
        style={{
          width: "100%",
          borderCollapse: "collapse",
          fontFamily: "var(--font-mono)",
          fontSize: "13px",
        }}
      >
        <thead>
          <tr>
            {columns.map((col) => (
              <th
                key={col}
                style={{
                  position: "sticky",
                  top: 0,
                  padding: "8px 12px",
                  textAlign: "left",
                  background: "var(--bg-secondary)",
                  color: "var(--accent)",
                  borderBottom: "1px solid var(--border)",
                  fontWeight: 600,
                  whiteSpace: "nowrap",
                }}
              >
                {col}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, i) => (
            <tr
              key={i}
              style={{
                borderBottom: "1px solid var(--border)",
              }}
              onMouseEnter={(e) =>
                (e.currentTarget.style.background = "var(--bg-hover)")
              }
              onMouseLeave={(e) =>
                (e.currentTarget.style.background = "transparent")
              }
            >
              {row.map((cell, j) => (
                <td
                  key={j}
                  style={{
                    padding: "6px 12px",
                    color: cell === null ? "var(--text-muted)" : "var(--text-primary)",
                    fontStyle: cell === null ? "italic" : "normal",
                    whiteSpace: "nowrap",
                    maxWidth: "400px",
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                  }}
                >
                  {formatCell(cell)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
