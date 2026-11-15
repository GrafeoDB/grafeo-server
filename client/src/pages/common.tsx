import { useState, type ReactNode } from "react";
import { ConfirmDialog, FORBIDDEN_NOTE, UNAUTHENTICATED_NOTE } from "@grafeo-db/ui";
import { GrafeoApiError, refusedKind } from "../api/client";
import { formatBytes } from "../shell/format";

export function Chip({ children, tone }: { children: ReactNode; tone?: "strong" | "ram" | "disk" | "none" }) {
  return <span className={tone ? `st-chip st-chip-${tone}` : "st-chip"}>{children}</span>;
}

export type StatItem = [label: string, value: ReactNode, unit?: string];

/** Figures split by hairlines. */
export function Stats({ items }: { items: StatItem[] }) {
  return (
    <div className="st-stats">
      {items.map(([label, value, unit]) => (
        <div className="st-stat" key={label}>
          <span className="st-stat-label">{label}</span>
          <div className="st-stat-value">
            {value}
            {unit ? <small>{unit}</small> : null}
          </div>
        </div>
      ))}
    </div>
  );
}

/** A byte count as a stat: the figure and its unit. */
export function bytesStat(label: string, bytes: number | undefined | null): StatItem {
  if (bytes == null) return [label, "…"];
  const [value, unit] = formatBytes(bytes);
  return [label, value, unit];
}

export interface Ask {
  title: string;
  body: ReactNode;
  confirmLabel: string;
  danger?: boolean;
  confirmText?: string;
  onConfirm(): void;
}

/** One ConfirmDialog for every question on a page. */
export function useAsk() {
  const [ask, setAsk] = useState<Ask | null>(null);
  const dialog = (
    <ConfirmDialog
      open={ask !== null}
      title={ask?.title ?? ""}
      body={ask?.body ?? null}
      confirmLabel={ask?.confirmLabel ?? ""}
      danger={ask?.danger}
      confirmText={ask?.confirmText}
      onConfirm={() => {
        const current = ask;
        setAsk(null);
        current?.onConfirm();
      }}
      onCancel={() => setAsk(null)}
    />
  );
  return { open: setAsk, dialog };
}

/** What a failed request says: a refusal (401/403) in the package's words, else the server's detail. */
export function errorMessage(err: unknown): string {
  const kind = refusedKind(err);
  if (kind) return kind === "forbidden" ? FORBIDDEN_NOTE : UNAUTHENTICATED_NOTE;
  return err instanceof GrafeoApiError ? err.detail : err instanceof Error ? err.message : String(err);
}

/** An action's failure, said under the page header until the next action. */
export function ActionError({ message }: { message: string | null }) {
  if (!message) return null;
  return (
    <p className="st-error" role="alert">
      {message}
    </p>
  );
}
