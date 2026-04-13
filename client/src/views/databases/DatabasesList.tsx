import { useCallback, useState } from "react";
import ServerStats from "../../components/Databases/ServerStats";
import DatabaseCards from "../../components/Databases/DatabaseCards";
import CreateDatabaseDialog from "../../components/Sidebar/CreateDatabaseDialog";
import btn from "../../styles/buttons.module.css";
import styles from "./DatabasesList.module.css";

export default function DatabasesList() {
  const [creating, setCreating] = useState(false);
  const [refreshKey, setRefreshKey] = useState(0);

  const handleCreated = useCallback(() => {
    setCreating(false);
    setRefreshKey((k) => k + 1);
  }, []);

  return (
    <div className={styles.page}>
      <ServerStats />
      <div className={styles.header}>
        <h2 className={styles.heading}>Databases</h2>
        <button
          type="button"
          className={btn.secondary}
          onClick={() => setCreating(true)}
        >
          + New database
        </button>
      </div>
      <DatabaseCards key={refreshKey} />
      <CreateDatabaseDialog
        open={creating}
        onClose={() => setCreating(false)}
        onCreated={handleCreated}
      />
    </div>
  );
}
