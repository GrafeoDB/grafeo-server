import type { QueryResponse } from "../../types/api";
import styles from "./StatusBar.module.css";

interface StatusBarProps {
  result: QueryResponse | null;
  error: string | null;
  isLoading: boolean;
  onShowShortcuts: () => void;
}

export default function StatusBar({ result, error, isLoading, onShowShortcuts }: StatusBarProps) {
  return (
    <div className={styles.container}>
      {isLoading && <span className={styles.loading}>Executing query...</span>}
      {error && <span className={styles.error}>{error}</span>}
      {result && !isLoading && !error && (
        <span className={styles.info}>
          {result.rows.length} row{result.rows.length !== 1 ? "s" : ""}
          {result.execution_time_ms != null && (
            <> &middot; {result.execution_time_ms.toFixed(1)}ms</>
          )}
          {result.rows_scanned != null && (
            <> &middot; {result.rows_scanned} scanned</>
          )}
        </span>
      )}
      {!result && !error && !isLoading && (
        <span className={styles.muted}>Ready</span>
      )}
      <button
        className={styles.shortcutButton}
        onClick={onShowShortcuts}
        title="Keyboard shortcuts"
      >
        ?
      </button>
    </div>
  );
}
