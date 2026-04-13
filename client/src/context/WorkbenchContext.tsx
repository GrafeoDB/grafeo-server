import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useQuery } from "../hooks/useQuery";
import { useQueryHistory } from "../hooks/useQueryHistory";
import type { HistoryEntry } from "../hooks/useQueryHistory";
import type { QueryEditorHandle } from "../components/QueryEditor/QueryEditor";
import type { Tab } from "../components/QueryEditor/TabBar";
import { useApp } from "./AppContext";

export interface SavedQuery {
  name: string;
  query: string;
}

interface TabState {
  tabs: Tab[];
  activeTabId: string;
}

export interface WorkbenchContextValue {
  // Editor ref, owned here so sidebar-triggered query selection can write to it.
  editorRef: React.RefObject<QueryEditorHandle | null>;

  // Tab state
  tabState: TabState;
  activeLanguage: string;
  onSelectTab: (id: string) => void;
  onAddTab: () => void;
  onCloseTab: (id: string) => void;
  onRenameTab: (id: string, name: string) => void;
  onLanguageChange: (lang: string) => void;

  // Query execution
  result: ReturnType<typeof useQuery>["result"];
  error: ReturnType<typeof useQuery>["error"];
  isLoading: boolean;
  onExecute: (query: string) => void;

  // Query history
  historyEntries: HistoryEntry[];
  historyNavigateUp: () => HistoryEntry | null;
  historyNavigateDown: () => HistoryEntry | null;
  historyResetCursor: () => void;

  // Saved queries
  savedQueries: SavedQuery[];
  onSaveQuery: (query: string) => void;
  onRemoveSaved: (index: number) => void;

  // Cross-boundary action: sidebar clicks a saved query or history entry,
  // this writes it into the active tab and executes it.
  onQuerySelect: (query: string) => void;

  // Results view mode
  viewMode: "table" | "graph";
  setViewMode: (mode: "table" | "graph") => void;

  // Shortcut help modal
  showHelp: boolean;
  toggleHelp: () => void;
}

const WorkbenchContext = createContext<WorkbenchContextValue | null>(null);

const SAVED_KEY = "grafeo-saved-queries";
const TABS_KEY = "grafeo-editor-tabs";

function generateId(): string {
  return crypto.randomUUID();
}

function loadSaved(): SavedQuery[] {
  try {
    const raw = localStorage.getItem(SAVED_KEY);
    return raw ? JSON.parse(raw) : [];
  } catch {
    return [];
  }
}

function persistSaved(queries: SavedQuery[]) {
  localStorage.setItem(SAVED_KEY, JSON.stringify(queries));
}

function loadTabs(): TabState {
  try {
    const raw = localStorage.getItem(TABS_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as TabState;
      if (parsed.tabs?.length > 0 && parsed.activeTabId) return parsed;
    }
  } catch {
    /* use default */
  }
  const id = generateId();
  return { tabs: [{ id, name: "Query 1", language: "gql" }], activeTabId: id };
}

function persistTabs(state: TabState) {
  localStorage.setItem(TABS_KEY, JSON.stringify(state));
}

interface Props {
  children: ReactNode;
}

export function WorkbenchProvider({ children }: Props) {
  const { currentDatabase, databaseType } = useApp();
  const [tabState, setTabState] = useState<TabState>(loadTabs);
  const [viewMode, setViewMode] = useState<"table" | "graph">("graph");
  const { result, error, isLoading, execute } = useQuery();
  const history = useQueryHistory();
  const [savedQueries, setSavedQueries] = useState<SavedQuery[]>(loadSaved);
  const [showHelp, setShowHelp] = useState(false);
  const editorRef = useRef<QueryEditorHandle>(null);

  const activeTab =
    tabState.tabs.find((t) => t.id === tabState.activeTabId) ?? tabState.tabs[0];
  const activeLanguage = activeTab.language;

  const onLanguageChange = useCallback((lang: string) => {
    setTabState((prev) => {
      const next = {
        ...prev,
        tabs: prev.tabs.map((t) =>
          t.id === prev.activeTabId ? { ...t, language: lang } : t,
        ),
      };
      persistTabs(next);
      return next;
    });
  }, []);

  // Auto-switch language when the current database switches graph models.
  useEffect(() => {
    const isRdf =
      databaseType === "rdf" ||
      databaseType === "owl-schema" ||
      databaseType === "rdfs-schema";
    const validLangs = isRdf
      ? new Set(["gql", "sparql"])
      : new Set(["gql", "cypher", "graphql", "gremlin"]);
    if (!validLangs.has(activeLanguage)) {
      onLanguageChange(isRdf ? "sparql" : "gql");
    }
  }, [databaseType, activeLanguage, onLanguageChange]);

  const onSelectTab = useCallback((id: string) => {
    const content = editorRef.current?.getContent() ?? "";
    setTabState((prev) => {
      if (prev.activeTabId === id) return prev;
      sessionStorage.setItem(`grafeo-editor-tab-${prev.activeTabId}`, content);
      const next = { ...prev, activeTabId: id };
      persistTabs(next);
      return next;
    });
  }, []);

  useEffect(() => {
    const saved = sessionStorage.getItem(
      `grafeo-editor-tab-${tabState.activeTabId}`,
    );
    editorRef.current?.setContent(saved ?? "");
  }, [tabState.activeTabId]);

  const onAddTab = useCallback(() => {
    const content = editorRef.current?.getContent() ?? "";
    setTabState((prev) => {
      sessionStorage.setItem(`grafeo-editor-tab-${prev.activeTabId}`, content);
      const id = generateId();
      const num = prev.tabs.length + 1;
      const tab: Tab = { id, name: `Query ${num}`, language: "gql" };
      const next = { tabs: [...prev.tabs, tab], activeTabId: id };
      persistTabs(next);
      return next;
    });
  }, []);

  const onCloseTab = useCallback((id: string) => {
    setTabState((prev) => {
      if (prev.tabs.length <= 1) return prev;
      const idx = prev.tabs.findIndex((t) => t.id === id);
      const next = prev.tabs.filter((t) => t.id !== id);
      sessionStorage.removeItem(`grafeo-editor-tab-${id}`);
      let newActiveId = prev.activeTabId;
      if (id === prev.activeTabId) {
        newActiveId = next[Math.min(idx, next.length - 1)].id;
      }
      const state = { tabs: next, activeTabId: newActiveId };
      persistTabs(state);
      return state;
    });
  }, []);

  const onRenameTab = useCallback((id: string, name: string) => {
    setTabState((prev) => {
      const next = {
        ...prev,
        tabs: prev.tabs.map((t) => (t.id === id ? { ...t, name } : t)),
      };
      persistTabs(next);
      return next;
    });
  }, []);

  const onExecute = useCallback(
    (query: string) => {
      execute(query, activeLanguage, currentDatabase);
      history.add(query, activeLanguage);
    },
    [execute, activeLanguage, currentDatabase, history],
  );

  const onQuerySelect = useCallback(
    (query: string) => {
      editorRef.current?.setContent(query);
      execute(query, activeLanguage, currentDatabase);
      history.add(query, activeLanguage);
    },
    [execute, activeLanguage, currentDatabase, history],
  );

  const onSaveQuery = useCallback((query: string) => {
    const name = window.prompt("Save query as:");
    if (!name?.trim()) return;
    setSavedQueries((prev) => {
      const next = [...prev, { name: name.trim(), query }];
      persistSaved(next);
      return next;
    });
  }, []);

  const onRemoveSaved = useCallback((index: number) => {
    setSavedQueries((prev) => {
      const next = prev.filter((_, i) => i !== index);
      persistSaved(next);
      return next;
    });
  }, []);

  const toggleHelp = useCallback(() => {
    setShowHelp((prev) => !prev);
  }, []);

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key !== "?" || e.ctrlKey || e.metaKey || e.altKey) return;
      const tag = (e.target as HTMLElement).tagName;
      if (tag === "INPUT" || tag === "TEXTAREA") return;
      if ((e.target as HTMLElement).closest(".cm-editor")) return;
      e.preventDefault();
      setShowHelp((prev) => !prev);
    };
    document.addEventListener("keydown", handleKey);
    return () => document.removeEventListener("keydown", handleKey);
  }, []);

  const value = useMemo<WorkbenchContextValue>(
    () => ({
      editorRef,
      tabState,
      activeLanguage,
      onSelectTab,
      onAddTab,
      onCloseTab,
      onRenameTab,
      onLanguageChange,
      result,
      error,
      isLoading,
      onExecute,
      historyEntries: history.entries,
      historyNavigateUp: history.navigateUp,
      historyNavigateDown: history.navigateDown,
      historyResetCursor: history.resetCursor,
      savedQueries,
      onSaveQuery,
      onRemoveSaved,
      onQuerySelect,
      viewMode,
      setViewMode,
      showHelp,
      toggleHelp,
    }),
    [
      tabState,
      activeLanguage,
      onSelectTab,
      onAddTab,
      onCloseTab,
      onRenameTab,
      onLanguageChange,
      result,
      error,
      isLoading,
      onExecute,
      history.entries,
      history.navigateUp,
      history.navigateDown,
      history.resetCursor,
      savedQueries,
      onSaveQuery,
      onRemoveSaved,
      onQuerySelect,
      viewMode,
      showHelp,
      toggleHelp,
    ],
  );

  return (
    <WorkbenchContext.Provider value={value}>
      {children}
    </WorkbenchContext.Provider>
  );
}

/**
 * Returns the workbench context. Returns null on routes that aren't wrapped
 * in a WorkbenchProvider (e.g. the admin route), so consumers that only
 * render partial UI can gate on the null.
 */
export function useWorkbench(): WorkbenchContextValue | null {
  return useContext(WorkbenchContext);
}

/**
 * Returns the workbench context and throws if it isn't available. Use this
 * inside components that only ever render on the workbench route (like
 * WorkbenchView itself).
 */
export function useWorkbenchRequired(): WorkbenchContextValue {
  const ctx = useContext(WorkbenchContext);
  if (!ctx) {
    throw new Error(
      "useWorkbenchRequired called outside a WorkbenchProvider. " +
        "Wrap the route element in <WorkbenchProvider>.",
    );
  }
  return ctx;
}
