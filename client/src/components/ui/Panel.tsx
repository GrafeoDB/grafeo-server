import type { ReactNode } from "react";
import styles from "./Panel.module.css";

interface PanelProps {
  children: ReactNode;
  className?: string;
}

/**
 * Framed container for tables and other content that needs a visible border.
 * Grid-based content (stat tiles, card grids) should sit directly on the
 * page background instead — don't wrap in Panel.
 */
export function Panel({ children, className }: PanelProps) {
  return (
    <div className={`${styles.panel} ${className ?? ""}`.trim()}>{children}</div>
  );
}
