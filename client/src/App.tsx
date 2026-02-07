import { useState, useRef, useCallback, useEffect } from "react";
import { useQuery } from "./hooks/useQuery";
import { useQueryHistory } from "./hooks/useQueryHistory";
import QueryEditor from "./components/QueryEditor/QueryEditor";
import type { QueryEditorHandle } from "./components/QueryEditor/QueryEditor";
import type { Tab } from "./components/QueryEditor/TabBar";
import ResultsPanel from "./components/ResultsPanel/ResultsPanel";
import Sidebar from "./components/Sidebar/Sidebar";
import type { SavedQuery } from "./components/Sidebar/Sidebar";
import StatusBar from "./components/StatusBar/StatusBar";
import ShortcutHelp from "./components/ShortcutHelp/ShortcutHelp";
import styles from "./App.module.css";

const SAVED_KEY = "grafeo-saved-queries";
const SIDEBAR_KEY = "grafeo-sidebar-open";
const TABS_KEY = "grafeo-editor-tabs";
const DB_KEY = "grafeo-current-database";

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

interface TabState {
  tabs: Tab[];
  activeTabId: string;
}

function loadTabs(): TabState {
  try {
    const raw = localStorage.getItem(TABS_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as TabState;
      if (parsed.tabs?.length > 0 && parsed.activeTabId) return parsed;
    }
  } catch { /* use default */ }
  const id = generateId();
  return { tabs: [{ id, name: "Query 1", language: "gql" }], activeTabId: id };
}

function persistTabs(state: TabState) {
  localStorage.setItem(TABS_KEY, JSON.stringify(state));
}

export default function App() {
  const [tabState, setTabState] = useState<TabState>(loadTabs);
  const [viewMode, setViewMode] = useState<"table" | "graph">("graph");
  const { result, error, isLoading, execute } = useQuery();
  const history = useQueryHistory();
  const [savedQueries, setSavedQueries] = useState<SavedQuery[]>(loadSaved);
  const [sidebarOpen, setSidebarOpen] = useState(
    () => localStorage.getItem(SIDEBAR_KEY) !== "false",
  );
  const [showHelp, setShowHelp] = useState(false);
  const [currentDatabase, setCurrentDatabase] = useState(
    () => localStorage.getItem(DB_KEY) || "default",
  );
  const editorRef = useRef<QueryEditorHandle>(null);

  const activeTab = tabState.tabs.find((t) => t.id === tabState.activeTabId) ?? tabState.tabs[0];
  const language = activeTab.language;

  const handleLanguageChange = useCallback((lang: string) => {
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

  const handleSelectTab = useCallback((id: string) => {
    // Save current content before switching
    const content = editorRef.current?.getContent() ?? "";
    setTabState((prev) => {
      if (prev.activeTabId === id) return prev;
      sessionStorage.setItem(`grafeo-editor-tab-${prev.activeTabId}`, content);
      const next = { ...prev, activeTabId: id };
      persistTabs(next);
      return next;
    });
  }, []);

  // When activeTabId changes, load that tab's content
  useEffect(() => {
    const saved = sessionStorage.getItem(`grafeo-editor-tab-${tabState.activeTabId}`);
    editorRef.current?.setContent(saved ?? "");
  }, [tabState.activeTabId]);

  const handleAddTab = useCallback(() => {
    // Save current tab content first
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

  const handleCloseTab = useCallback((id: string) => {
    setTabState((prev) => {
      if (prev.tabs.length <= 1) return prev;
      const idx = prev.tabs.findIndex((t) => t.id === id);
      const next = prev.tabs.filter((t) => t.id !== id);
      sessionStorage.removeItem(`grafeo-editor-tab-${id}`);
      let newActiveId = prev.activeTabId;
      if (id === prev.activeTabId) {
        // Switch to neighbor tab
        newActiveId = next[Math.min(idx, next.length - 1)].id;
      }
      const state = { tabs: next, activeTabId: newActiveId };
      persistTabs(state);
      return state;
    });
  }, []);

  const handleRenameTab = useCallback((id: string, name: string) => {
    setTabState((prev) => {
      const next = {
        ...prev,
        tabs: prev.tabs.map((t) => (t.id === id ? { ...t, name } : t)),
      };
      persistTabs(next);
      return next;
    });
  }, []);

  const handleSelectDatabase = useCallback((name: string) => {
    setCurrentDatabase(name);
    localStorage.setItem(DB_KEY, name);
  }, []);

  const handleExecute = useCallback(
    (query: string) => {
      execute(query, language, currentDatabase);
      history.add(query, language);
    },
    [execute, language, currentDatabase, history],
  );

  const handleQuerySelect = useCallback(
    (query: string) => {
      editorRef.current?.setContent(query);
      execute(query, language, currentDatabase);
      history.add(query, language);
    },
    [execute, language, currentDatabase, history],
  );

  const handleSaveQuery = useCallback((query: string) => {
    const name = window.prompt("Save query as:");
    if (!name?.trim()) return;
    setSavedQueries((prev) => {
      const next = [...prev, { name: name.trim(), query }];
      persistSaved(next);
      return next;
    });
  }, []);

  const handleRemoveSaved = useCallback((index: number) => {
    setSavedQueries((prev) => {
      const next = prev.filter((_, i) => i !== index);
      persistSaved(next);
      return next;
    });
  }, []);

  const handleToggleHelp = useCallback(() => {
    setShowHelp((prev) => !prev);
  }, []);

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key !== "?" || e.ctrlKey || e.metaKey || e.altKey) return;
      const tag = (e.target as HTMLElement).tagName;
      if (tag === "INPUT" || tag === "TEXTAREA") return;
      // Don't trigger when typing in CodeMirror
      if ((e.target as HTMLElement).closest(".cm-editor")) return;
      e.preventDefault();
      setShowHelp((prev) => !prev);
    };
    document.addEventListener("keydown", handleKey);
    return () => document.removeEventListener("keydown", handleKey);
  }, []);

  const handleToggleSidebar = useCallback(() => {
    setSidebarOpen((prev) => {
      const next = !prev;
      localStorage.setItem(SIDEBAR_KEY, String(next));
      return next;
    });
  }, []);

  return (
    <div className={styles.layout}>
      <Sidebar
        onQuerySelect={handleQuerySelect}
        savedQueries={savedQueries}
        onRemoveSaved={handleRemoveSaved}
        historyEntries={history.entries}
        collapsed={!sidebarOpen}
        onToggle={handleToggleSidebar}
        currentDatabase={currentDatabase}
        onSelectDatabase={handleSelectDatabase}
      />
      <div className={styles.main}>
        <QueryEditor
          ref={editorRef}
          language={language}
          onLanguageChange={handleLanguageChange}
          onExecute={handleExecute}
          onSave={handleSaveQuery}
          isLoading={isLoading}
          onHistoryUp={history.navigateUp}
          onHistoryDown={history.navigateDown}
          onHistoryReset={history.resetCursor}
          tabs={tabState.tabs}
          activeTabId={tabState.activeTabId}
          onSelectTab={handleSelectTab}
          onAddTab={handleAddTab}
          onCloseTab={handleCloseTab}
          onRenameTab={handleRenameTab}
          currentDatabase={currentDatabase}
        />
        <ResultsPanel
          result={result}
          viewMode={viewMode}
          onViewModeChange={setViewMode}
        />
        <StatusBar result={result} error={error} isLoading={isLoading} onShowShortcuts={handleToggleHelp} />
      </div>
      {showHelp && <ShortcutHelp onClose={handleToggleHelp} />}
    </div>
  );
}
