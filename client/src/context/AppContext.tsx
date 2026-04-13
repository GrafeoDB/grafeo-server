import { createContext, useContext, useState, useCallback, type ReactNode } from "react";

const DB_KEY = "grafeo-current-database";
const SIDEBAR_KEY = "grafeo-sidebar-open";

interface AppContextValue {
  currentDatabase: string;
  databaseType: string;
  sidebarOpen: boolean;
  setDatabaseType: (t: string) => void;
  selectDatabase: (name: string, dbType?: string) => void;
  toggleSidebar: () => void;
}

const AppContext = createContext<AppContextValue | null>(null);

export function AppProvider({ children }: { children: ReactNode }) {
  const [currentDatabase, setCurrentDatabase] = useState(
    () => localStorage.getItem(DB_KEY) || "default",
  );
  const [databaseType, setDatabaseType] = useState("lpg");
  const [sidebarOpen, setSidebarOpen] = useState(
    () => localStorage.getItem(SIDEBAR_KEY) !== "false",
  );

  const selectDatabase = useCallback((name: string, dbType?: string) => {
    setCurrentDatabase(name);
    localStorage.setItem(DB_KEY, name);
    if (dbType) setDatabaseType(dbType);
  }, []);

  const toggleSidebar = useCallback(() => {
    setSidebarOpen((prev) => {
      const next = !prev;
      localStorage.setItem(SIDEBAR_KEY, String(next));
      return next;
    });
  }, []);

  return (
    <AppContext.Provider
      value={{
        currentDatabase,
        databaseType,
        sidebarOpen,
        setDatabaseType,
        selectDatabase,
        toggleSidebar,
      }}
    >
      {children}
    </AppContext.Provider>
  );
}

export function useApp(): AppContextValue {
  const ctx = useContext(AppContext);
  if (!ctx) throw new Error("useApp must be used within AppProvider");
  return ctx;
}
