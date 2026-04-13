import { useState } from "react";
import DatabasePanel from "./DatabasePanel";
import { useWorkbenchRequired } from "../../context/WorkbenchContext";
import styles from "./Sidebar.module.css";

interface SidebarProps {
  collapsed: boolean;
  onToggle: () => void;
  currentDatabase: string;
  onSelectDatabase: (name: string, dbType?: string) => void;
}

const EXAMPLES = [
  { name: "All nodes", query: "MATCH (n) RETURN n LIMIT 25" },
  { name: "All edges", query: "MATCH (a)-[r]->(b) RETURN a, r, b LIMIT 25" },
  { name: "Count nodes", query: "MATCH (n) RETURN count(n)" },
  { name: "Node labels", query: "MATCH (n) RETURN DISTINCT labels(n)" },
  { name: "Insert person", query: "INSERT (:Person {name: 'Alice', age: 30})" },
  { name: "Find by name", query: "MATCH (p:Person {name: 'Alice'}) RETURN p" },
];

function formatTimeAgo(timestamp: number): string {
  const diff = Math.floor((Date.now() - timestamp) / 1000);
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return `${Math.floor(diff / 86400)}d ago`;
}

const MAX_VISIBLE_HISTORY = 10;

export default function Sidebar({
  collapsed,
  onToggle,
  currentDatabase,
  onSelectDatabase,
}: SidebarProps) {
  const [showAllHistory, setShowAllHistory] = useState(false);
  const workbench = useWorkbenchRequired();

  const visibleHistory = showAllHistory
    ? workbench.historyEntries
    : workbench.historyEntries.slice(0, MAX_VISIBLE_HISTORY);

  return (
    <aside className={`${styles.container} ${collapsed ? styles.collapsed : ""}`}>
      <div className={styles.collapseBar}>
        <button
          className={styles.collapseButton}
          onClick={onToggle}
          title={collapsed ? "Expand sidebar" : "Collapse sidebar"}
        >
          {collapsed ? "\u00BB" : "\u00AB"}
        </button>
      </div>

      {!collapsed && (
        <>
          <div className={styles.section}>
            <h3 className={styles.heading}>Databases</h3>
            <DatabasePanel
              currentDatabase={currentDatabase}
              onSelectDatabase={onSelectDatabase}
            />
          </div>

          {workbench.savedQueries.length > 0 && (
            <div className={styles.section}>
              <h3 className={styles.heading}>Saved Queries</h3>
              <ul className={styles.list}>
                {workbench.savedQueries.map((q, i) => (
                  <li key={i} className={styles.item}>
                    <button
                      className={styles.itemButton}
                      onClick={() => workbench.onQuerySelect(q.query)}
                      title={q.query}
                    >
                      {q.name}
                    </button>
                    <button
                      className={styles.removeButton}
                      onClick={() => workbench.onRemoveSaved(i)}
                    >
                      x
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}

          {workbench.historyEntries.length > 0 && (
            <div className={styles.section}>
              <h3 className={styles.heading}>History</h3>
              <ul className={styles.list}>
                {visibleHistory.map((entry, i) => (
                  <li key={i} className={styles.item}>
                    <button
                      className={styles.itemButton}
                      onClick={() => workbench.onQuerySelect(entry.query)}
                      title={entry.query}
                    >
                      <span className={styles.historyQuery}>
                        {entry.query.length > 40
                          ? entry.query.slice(0, 40) + "..."
                          : entry.query}
                      </span>
                      <span className={styles.historyTime}>
                        {formatTimeAgo(entry.timestamp)}
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
              {workbench.historyEntries.length > MAX_VISIBLE_HISTORY && (
                <button
                  className={styles.showMoreButton}
                  onClick={() => setShowAllHistory(!showAllHistory)}
                >
                  {showAllHistory
                    ? "Show less"
                    : `Show all (${workbench.historyEntries.length})`}
                </button>
              )}
            </div>
          )}

          <div className={styles.section}>
            <h3 className={styles.heading}>Examples</h3>
            <ul className={styles.list}>
              {EXAMPLES.map((q) => (
                <li key={q.name} className={styles.item}>
                  <button
                    className={styles.itemButton}
                    onClick={() => workbench.onQuerySelect(q.query)}
                    title={q.query}
                  >
                    {q.name}
                  </button>
                </li>
              ))}
            </ul>
          </div>
        </>
      )}
    </aside>
  );
}
