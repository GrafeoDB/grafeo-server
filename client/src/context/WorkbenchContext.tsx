import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { LanguageId, MainView, QueryResult, QueryTab, Refusal } from "@grafeo-db/ui";
import { api, GrafeoApiError, refusedKind } from "../api/client";
import type { QueryRequest, QueryResponse } from "../types/api";
import { toPlan, toQueryResult } from "../adapter/toQueryResult";
import { isRdfModel, useServer } from "./ServerContext";
import { readJson, readStored, removeStored, writeJson, writeStored } from "../storage";

export interface SavedQuery {
  name: string;
  query: string;
  language?: LanguageId;
}

export interface HistoryEntry {
  query: string;
  language: string;
  timestamp: number;
}

export interface WorkbenchContextValue {
  tabs: QueryTab[];
  activeTab: QueryTab;
  /** The text of the active tab. */
  text: string;
  setText(text: string): void;
  selectTab(id: string): void;
  addTab(): void;
  closeTab(id: string): void;

  /** The active tab's language when the active database runs it, else that model's default. */
  language: LanguageId;
  /** The languages the active database runs, as the server offers them. */
  languages: LanguageId[];
  setLanguage(language: LanguageId): void;

  run(): void;
  /** Explain, when the language has a plan (GQL and Cypher). */
  explain: (() => void) | undefined;
  running: boolean;
  result: QueryResult | null;
  /** The rows as the server sent them, for the JSON view. */
  rawRows: unknown[][] | null;
  error: string | null;
  refused: Refusal | undefined;
  /** The database and language of the last run, for the status bar. */
  lastRun: { database: string; language: LanguageId } | null;

  selectedId: string | null;
  setSelectedId(id: string | null): void;
  mainView: MainView;
  setMainView(view: MainView): void;
  resultsCollapsed: boolean;
  setResultsCollapsed(collapsed: boolean): void;

  history: HistoryEntry[];
  /** Ctrl+Up / Ctrl+Down: the previous or next history entry into the active tab. */
  historyStep(direction: 1 | -1): void;
  saved: SavedQuery[];
  saveQuery(name: string): void;
  removeSaved(index: number): void;
  /** A query from history, the examples or saved queries: into the active tab when it is empty, else a new tab. */
  openQuery(query: string, language?: LanguageId): void;
}

const WorkbenchContext = createContext<WorkbenchContextValue | null>(null);

const TABS_KEY = "grafeo-editor-tabs";
const SAVED_KEY = "grafeo-saved-queries";
const HISTORY_KEY = "grafeo-query-history";
const MAX_HISTORY = 50;
const tabTextKey = (id: string) => `grafeo-editor-tab-${id}`;

const ALL_LANGUAGES: LanguageId[] = ["gql", "cypher", "gremlin", "graphql", "sparql", "sql"];
const LPG_LANGUAGES = new Set<LanguageId>(["gql", "cypher", "gremlin", "graphql", "sql"]);
const RDF_LANGUAGES = new Set<LanguageId>(["gql", "sparql"]);
const EXPLAINABLE = new Set<LanguageId>(["gql", "cypher"]);

/** The server's feature names for the languages, where they differ from the editor's. */
const FEATURE_NAME: Partial<Record<LanguageId, string>> = { sql: "sql-pgq" };

interface TabState {
  tabs: QueryTab[];
  activeTabId: string;
}

function isLanguage(value: unknown): value is LanguageId {
  return typeof value === "string" && (ALL_LANGUAGES as string[]).includes(value);
}

function isTabState(value: unknown): value is TabState {
  if (typeof value !== "object" || value === null) return false;
  const v = value as TabState;
  return (
    Array.isArray(v.tabs) &&
    v.tabs.length > 0 &&
    v.tabs.every((t) => typeof t?.id === "string" && typeof t.name === "string" && isLanguage(t.language)) &&
    v.tabs.some((t) => t.id === v.activeTabId)
  );
}

function isSavedList(value: unknown): value is SavedQuery[] {
  return Array.isArray(value) && value.every((q) => typeof q?.name === "string" && typeof q.query === "string");
}

function isHistory(value: unknown): value is HistoryEntry[] {
  return Array.isArray(value) && value.every((e) => typeof e?.query === "string" && typeof e.timestamp === "number");
}

/**
 * A random tab id. `crypto.randomUUID` exists only in secure contexts (HTTPS or localhost); the Studio is also opened
 * over plain HTTP on a LAN address, where `getRandomValues` still works.
 */
function randomId(): string {
  if (typeof crypto.randomUUID === "function") return crypto.randomUUID();
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

function newTab(tabs: QueryTab[], language: LanguageId = "gql"): QueryTab {
  const used = new Set(tabs.map((t) => t.name));
  let n = tabs.length + 1;
  while (used.has(`Query ${n}`)) n++;
  return { id: randomId(), name: `Query ${n}`, language };
}

function loadTabs(): TabState {
  const stored = readJson<TabState | null>(TABS_KEY, null, (v): v is TabState | null => isTabState(v));
  if (stored) return stored;
  const tab = newTab([]);
  return { tabs: [tab], activeTabId: tab.id };
}

const FIRST_QUERY = "MATCH (n) RETURN n LIMIT 25";

export function WorkbenchProvider({ children }: { children: ReactNode }) {
  const { activeDatabase, activeModel, health, refreshSchema, refreshDatabases } = useServer();
  const [tabState, setTabState] = useState<TabState>(loadTabs);
  const [texts, setTexts] = useState<Record<string, string>>(() => {
    const out: Record<string, string> = {};
    for (const tab of tabState.tabs) out[tab.id] = readStored(tabTextKey(tab.id), "session") ?? "";
    if (tabState.tabs.length === 1 && !out[tabState.activeTabId]) out[tabState.activeTabId] = FIRST_QUERY;
    return out;
  });
  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<QueryResult | null>(null);
  const [raw, setRaw] = useState<QueryResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [refused, setRefused] = useState<Refusal | undefined>();
  const [lastRun, setLastRun] = useState<WorkbenchContextValue["lastRun"]>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [mainView, setMainView] = useState<MainView>("graph");
  const [resultsCollapsed, setResultsCollapsed] = useState(false);
  const [history, setHistory] = useState<HistoryEntry[]>(() => readJson(HISTORY_KEY, [], isHistory));
  const [saved, setSaved] = useState<SavedQuery[]>(() => readJson(SAVED_KEY, [], isSavedList));
  const historyCursor = useRef(-1);
  // The text before Ctrl+Up stepped into the history, given back when stepping past the newest entry.
  const historyDraft = useRef("");
  // The run that may still answer; an older one finishing late is ignored.
  const runId = useRef(0);
  // The database a late answer is checked against: one for another database is never shown.
  const databaseRef = useRef(activeDatabase);
  useEffect(() => {
    databaseRef.current = activeDatabase;
  }, [activeDatabase]);

  const activeTab = tabState.tabs.find((t) => t.id === tabState.activeTabId) ?? tabState.tabs[0];
  const text = texts[activeTab.id] ?? "";

  const persistTabs = useCallback((state: TabState) => writeJson(TABS_KEY, state), []);

  const writeText = useCallback(
    (value: string) => {
      setTexts((prev) => ({ ...prev, [activeTab.id]: value }));
      writeStored(tabTextKey(activeTab.id), value, "session");
    },
    [activeTab.id],
  );
  // An edit by hand starts history stepping over from the newest entry.
  const setText = useCallback(
    (value: string) => {
      historyCursor.current = -1;
      writeText(value);
    },
    [writeText],
  );

  const updateTabs = useCallback(
    (update: (prev: TabState) => TabState) => {
      setTabState((prev) => {
        const next = update(prev);
        if (next !== prev) persistTabs(next);
        return next;
      });
    },
    [persistTabs],
  );

  const selectTab = useCallback(
    (id: string) => {
      historyCursor.current = -1;
      updateTabs((prev) => (prev.activeTabId === id ? prev : { ...prev, activeTabId: id }));
    },
    [updateTabs],
  );

  const addTabWith = useCallback(
    (query: string, language?: LanguageId) => {
      const tab = newTab(tabState.tabs, language ?? activeTab.language);
      setTexts((prev) => ({ ...prev, [tab.id]: query }));
      writeStored(tabTextKey(tab.id), query, "session");
      updateTabs((prev) => ({ tabs: [...prev.tabs, tab], activeTabId: tab.id }));
    },
    [updateTabs, tabState.tabs, activeTab.language],
  );

  const addTab = useCallback(() => addTabWith(""), [addTabWith]);

  const closeTab = useCallback(
    (id: string) => {
      removeStored(tabTextKey(id), "session");
      setTexts((prev) => {
        const next = { ...prev };
        delete next[id];
        return next;
      });
      updateTabs((prev) => {
        const index = prev.tabs.findIndex((t) => t.id === id);
        if (index < 0) return prev;
        const tabs = prev.tabs.filter((t) => t.id !== id);
        // Closing the last tab leaves one empty tab, never none.
        if (tabs.length === 0) {
          const tab = newTab([]);
          return { tabs: [tab], activeTabId: tab.id };
        }
        const activeTabId = id === prev.activeTabId ? tabs[Math.min(index, tabs.length - 1)].id : prev.activeTabId;
        return { tabs, activeTabId };
      });
    },
    [updateTabs],
  );

  const rdf = isRdfModel(activeModel);
  const languages = useMemo(() => {
    const valid = rdf ? RDF_LANGUAGES : LPG_LANGUAGES;
    const offered = health?.features?.languages;
    return ALL_LANGUAGES.filter(
      (l) => valid.has(l) && (!offered || offered.includes(FEATURE_NAME[l] ?? l)),
    );
  }, [rdf, health]);
  const fallback: LanguageId = rdf ? "sparql" : "gql";
  const language = languages.includes(activeTab.language) ? activeTab.language : fallback;

  const setLanguage = useCallback(
    (lang: LanguageId) =>
      updateTabs((prev) => ({
        ...prev,
        tabs: prev.tabs.map((t) => (t.id === prev.activeTabId ? { ...t, language: lang } : t)),
      })),
    [updateTabs],
  );

  const addHistory = useCallback((query: string, lang: LanguageId) => {
    setHistory((prev) => {
      if (prev[0]?.query === query && prev[0]?.language === lang) return prev;
      const next = [{ query, language: lang, timestamp: Date.now() }, ...prev].slice(0, MAX_HISTORY);
      writeJson(HISTORY_KEY, next);
      return next;
    });
    historyCursor.current = -1;
  }, []);

  const execute = useCallback(
    async (query: string, explain: boolean) => {
      const statement = query.trim();
      if (!statement) return;
      const id = ++runId.current;
      const database = activeDatabase;
      const current = () => id === runId.current && databaseRef.current === database;
      setRunning(true);
      setError(null);
      setRefused(undefined);
      const body: QueryRequest = { query: explain ? `EXPLAIN ${statement}` : statement, language };
      if (activeDatabase !== "default") body.database = activeDatabase;
      try {
        const response = await api.query(body);
        if (!current()) return;
        const next = toQueryResult(response);
        if (explain) next.plan = toPlan(response);
        setResult(next);
        setRaw(response);
        // A write changes the schema and the counts the sidebar shows.
        if (Object.values(response.counters ?? {}).some((n) => n > 0)) {
          refreshSchema();
          refreshDatabases();
        }
        setSelectedId(null);
        if (explain) setMainView("plan");
        else if (mainView === "plan") setMainView("graph");
      } catch (err) {
        if (!current()) return;
        // A failed or refused run drops the old result, so nothing stale stays on screen.
        setResult(null);
        setRaw(null);
        setSelectedId(null);
        const kind = refusedKind(err);
        if (kind) setRefused(kind);
        else setError(err instanceof GrafeoApiError ? err.detail : err instanceof Error ? err.message : String(err));
      } finally {
        if (current()) {
          setRunning(false);
          setLastRun({ database, language });
        }
      }
      if (!explain) addHistory(statement, language);
    },
    [language, activeDatabase, addHistory, mainView, refreshSchema, refreshDatabases],
  );

  const run = useCallback(() => void execute(text, false), [execute, text]);
  const explainFn = useCallback(() => void execute(text, true), [execute, text]);

  const historyStep = useCallback(
    (direction: 1 | -1) => {
      if (history.length === 0) return;
      const from = historyCursor.current;
      const next = Math.max(-1, Math.min(history.length - 1, from + direction));
      if (next === from) return;
      if (from === -1) historyDraft.current = text;
      historyCursor.current = next;
      writeText(next >= 0 ? history[next].query : historyDraft.current);
    },
    [history, text, writeText],
  );

  const saveQuery = useCallback(
    (name: string) => {
      const trimmed = name.trim();
      if (!trimmed || !text.trim()) return;
      setSaved((prev) => {
        const next = [...prev, { name: trimmed, query: text, language }];
        writeJson(SAVED_KEY, next);
        return next;
      });
    },
    [text, language],
  );

  const removeSaved = useCallback((index: number) => {
    setSaved((prev) => {
      const next = prev.filter((_, i) => i !== index);
      writeJson(SAVED_KEY, next);
      return next;
    });
  }, []);

  const openQuery = useCallback(
    (query: string, requested?: LanguageId) => {
      // A language the active database does not run (or an unknown one from an older Studio) is not taken over:
      // the tab shows, and runs, the language it has.
      const lang = isLanguage(requested) && languages.includes(requested) ? requested : undefined;
      if (!text.trim()) {
        setText(query);
        if (lang) setLanguage(lang);
      } else {
        addTabWith(query, lang ?? language);
      }
    },
    [text, setText, setLanguage, addTabWith, language, languages],
  );

  // A new database is a new graph: the old result and selection do not belong to it. Adjusted while rendering,
  // so no frame shows the old database's result.
  const [shownDatabase, setShownDatabase] = useState(activeDatabase);
  if (shownDatabase !== activeDatabase) {
    setShownDatabase(activeDatabase);
    setRunning(false);
    setResult(null);
    setRaw(null);
    setSelectedId(null);
    setError(null);
    setRefused(undefined);
  }

  const value = useMemo<WorkbenchContextValue>(
    () => ({
      tabs: tabState.tabs,
      activeTab,
      text,
      setText,
      selectTab,
      addTab,
      closeTab,
      language,
      languages,
      setLanguage,
      run,
      explain: EXPLAINABLE.has(language) ? explainFn : undefined,
      running,
      result,
      rawRows: raw?.rows ?? null,
      error,
      refused,
      lastRun,
      selectedId,
      setSelectedId,
      mainView,
      setMainView,
      resultsCollapsed,
      setResultsCollapsed,
      history,
      historyStep,
      saved,
      saveQuery,
      removeSaved,
      openQuery,
    }),
    [
      tabState.tabs, activeTab, text, setText, selectTab, addTab, closeTab, language, languages, setLanguage, run,
      explainFn, running, result, raw, error, refused, lastRun, selectedId, mainView, resultsCollapsed, history,
      historyStep, saved, saveQuery, removeSaved, openQuery,
    ],
  );
  return <WorkbenchContext.Provider value={value}>{children}</WorkbenchContext.Provider>;
}

export function useWorkbench(): WorkbenchContextValue {
  const ctx = useContext(WorkbenchContext);
  if (!ctx) throw new Error("useWorkbench must be used within WorkbenchProvider");
  return ctx;
}
