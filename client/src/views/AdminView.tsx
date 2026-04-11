import { useState, useEffect } from "react";
import { api } from "../api/client";
import TokenPanel from "../components/Admin/TokenPanel";
import styles from "./AdminView.module.css";

export default function AdminView() {
  const [hasAuth, setHasAuth] = useState(false);

  useEffect(() => {
    api.health().then((h) => {
      const features = h as unknown as Record<string, unknown>;
      const serverFeatures = features.enabled_server_features;
      if (Array.isArray(serverFeatures) && serverFeatures.includes("auth")) {
        setHasAuth(true);
      }
    }).catch(() => {});
  }, []);

  return (
    <div className={styles.container}>
      {hasAuth ? (
        <TokenPanel />
      ) : (
        <p>Authentication is not enabled. Start the server with <code>--auth-token</code> to manage tokens.</p>
      )}
    </div>
  );
}
