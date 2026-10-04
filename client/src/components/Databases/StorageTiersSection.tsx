import { useCallback, useEffect, useState } from "react";
import { api, GrafeoApiError } from "../../api/client";
import type { SectionTierInfo } from "../../types/api";
import btn from "../../styles/buttons.module.css";
import styles from "./StorageTiersSection.module.css";

interface Props {
  database: string;
  /** Called after a reload so the parent page can refetch memory stats. */
  onMutated?: () => void;
}

const TIER_LABELS: Record<SectionTierInfo["tier"], string> = {
  in_memory: "In RAM",
  on_disk: "On disk",
  uninitialized: "Not loaded",
  unknown: "Unknown",
};

export default function StorageTiersSection({ database, onMutated }: Props) {
  const [tiers, setTiers] = useState<SectionTierInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [reloading, setReloading] = useState(false);
  const [toast, setToast] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    setLoading(true);
    api.admin
      .storageTiers(database)
      .then((res) => setTiers(res.tiers))
      .catch(() => setTiers([]))
      .finally(() => setLoading(false));
  }, [database]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    if (!toast) return;
    const id = window.setTimeout(() => setToast(null), 4000);
    return () => window.clearTimeout(id);
  }, [toast]);

  const spilled = tiers.filter((t) => t.tier === "on_disk").length;

  const reload = async () => {
    setReloading(true);
    setError(null);
    try {
      const res = await api.admin.reloadEligible(database);
      setToast(
        res.reloaded === 0
          ? "Nothing reloaded: no spilled section fits within the memory target."
          : `Reloaded ${res.reloaded} section${res.reloaded === 1 ? "" : "s"} into RAM.`,
      );
      refresh();
      onMutated?.();
    } catch (err) {
      setError(err instanceof GrafeoApiError ? err.detail : String(err));
    } finally {
      setReloading(false);
    }
  };

  return (
    <section className={styles.section}>
      <div className={styles.header}>
        <h3 className={styles.heading}>Storage tiers</h3>
        <button
          type="button"
          className={btn.secondary}
          onClick={reload}
          disabled={reloading || spilled === 0}
          title={
            spilled === 0
              ? "No section is on disk"
              : "Bring spilled sections back into RAM, up to 70% of the memory limit"
          }
        >
          {reloading ? "Reloading…" : "Reload spilled sections"}
        </button>
      </div>

      {toast && <div className={styles.toast}>{toast}</div>}
      {error && <div className={styles.error}>{error}</div>}

      {loading ? (
        <div className={styles.empty}>Loading…</div>
      ) : tiers.length === 0 ? (
        <div className={styles.empty}>No storage sections reported.</div>
      ) : (
        <div className={styles.tableWrap}>
          <table className={styles.table}>
            <thead>
              <tr>
                <th>Section</th>
                <th>Tier</th>
              </tr>
            </thead>
            <tbody>
              {tiers.map((t) => (
                <tr key={t.section}>
                  <td>{t.section}</td>
                  <td>
                    <span className={`${styles.tier} ${styles[t.tier] ?? ""}`}>
                      {TIER_LABELS[t.tier] ?? t.tier}
                    </span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}
