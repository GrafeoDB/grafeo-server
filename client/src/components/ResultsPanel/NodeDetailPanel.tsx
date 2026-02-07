import styles from "./NodeDetailPanel.module.css";

interface NodeDetailPanelProps {
  node: Record<string, unknown> | null;
}

function formatValue(val: unknown): string {
  if (val === null || val === undefined) return "null";
  if (typeof val === "object") return JSON.stringify(val);
  return String(val);
}

export default function NodeDetailPanel({ node }: NodeDetailPanelProps) {
  if (!node) {
    return (
      <div className={styles.panel}>
        <div className={styles.header}>Node Details</div>
        <div className={styles.empty}>Click a node to see its properties</div>
      </div>
    );
  }

  // Sort keys: id first, then labels/label, then rest alphabetically
  const keys = Object.keys(node);
  const priority = ["id", "labels", "label"];
  const sorted = [
    ...priority.filter((k) => keys.includes(k)),
    ...keys.filter((k) => !priority.includes(k)).sort(),
  ];

  return (
    <div className={styles.panel}>
      <div className={styles.header}>Node Details</div>
      <div className={styles.properties}>
        {sorted.map((key) => (
          <div key={key} className={styles.row}>
            <span className={styles.propKey}>{key}</span>
            <span className={styles.propValue}>{formatValue(node[key])}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
