import { useCallback, useMemo, type ReactNode } from "react";
import { CategoryProvider, type LabelStyles } from "@grafeo-db/ui";
import { useServer } from "./context/ServerContext";
import { readJson, removeStored, writeJson } from "./storage";

const PREFIX = "grafeo-studio-label-styles:";

function isLabelStyles(value: unknown): value is LabelStyles {
  return (
    typeof value === "object" &&
    value !== null &&
    !Array.isArray(value) &&
    Object.values(value).every((style) => typeof style === "object" && style !== null && !Array.isArray(style))
  );
}

/** Forget a database's label styles (when the database is deleted). */
export function forgetLabelStyles(database: string): void {
  removeStored(PREFIX + database);
}

/**
 * The label styles and colour slots of the active database. A style belongs to the label in its database, so the
 * provider's scope is the database: switching loads B's styles into a fresh registry, without remounting the Studio,
 * and B never inherits A's styles or slots. Unreadable stored styles are the automatic ones.
 */
export function LabelStylesScope({ children }: { children: ReactNode }) {
  const { activeDatabase } = useServer();
  const key = PREFIX + activeDatabase;
  const initial = useMemo(() => readJson<LabelStyles>(key, {}, isLabelStyles), [key]);
  const onStylesChange = useCallback((styles: LabelStyles) => writeJson(key, styles), [key]);
  return (
    <CategoryProvider scope={activeDatabase} initialStyles={initial} onStylesChange={onStylesChange}>
      {children}
    </CategoryProvider>
  );
}
