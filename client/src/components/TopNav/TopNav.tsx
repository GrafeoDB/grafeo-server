import { useEffect, useState } from "react";
import { Link, useLocation } from "react-router-dom";
import { api } from "../../api/client";
import type { HealthResponse } from "../../types/api";
import styles from "./TopNav.module.css";

const POLL_INTERVAL_MS = 15_000;

export default function TopNav() {
  const location = useLocation();
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [reachable, setReachable] = useState<boolean | null>(null);

  useEffect(() => {
    let cancelled = false;
    const probe = async () => {
      try {
        const h = await api.health();
        if (!cancelled) {
          setHealth(h);
          setReachable(true);
        }
      } catch {
        if (!cancelled) {
          setHealth(null);
          setReachable(false);
        }
      }
    };
    probe();
    const id = window.setInterval(probe, POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, []);

  const hasAuth = health?.features?.server?.includes("auth") ?? false;
  const isStudio = location.pathname === "/" || location.pathname.startsWith("/studio");
  const isDatabases = location.pathname.startsWith("/databases");
  const isTokens = location.pathname.startsWith("/tokens");

  const statusLabel =
    reachable === null
      ? "Connecting..."
      : reachable
        ? `Connected · v${health?.version ?? "?"}`
        : "Disconnected";
  const statusClass =
    reachable === null
      ? styles.statusPending
      : reachable
        ? styles.statusOk
        : styles.statusDown;

  return (
    <header className={styles.topnav}>
      <div className={styles.brand}>
        <img
          src={import.meta.env.BASE_URL + "favicon.png"}
          alt="Grafeo"
          className={styles.logo}
        />
        <span className={styles.brandName}>Grafeo Studio</span>
      </div>

      <nav className={styles.nav}>
        <Link
          to="/"
          className={`${styles.navItem} ${isStudio && !isDatabases && !isTokens ? styles.navActive : ""}`}
        >
          Studio
        </Link>
        <Link
          to="/databases"
          className={`${styles.navItem} ${isDatabases ? styles.navActive : ""}`}
        >
          Databases
        </Link>
        {hasAuth && (
          <Link
            to="/tokens"
            className={`${styles.navItem} ${isTokens ? styles.navActive : ""}`}
          >
            Tokens
          </Link>
        )}
      </nav>

      <div className={styles.status} title={statusLabel}>
        <span className={`${styles.dot} ${statusClass}`} />
        <span className={styles.statusLabel}>{statusLabel}</span>
      </div>
    </header>
  );
}
