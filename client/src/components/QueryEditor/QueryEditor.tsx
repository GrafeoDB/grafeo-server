import { useEffect, useRef, useImperativeHandle, forwardRef } from "react";
import { EditorView, keymap, placeholder, ViewUpdate } from "@codemirror/view";
import { EditorState, Compartment } from "@codemirror/state";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { oneDark } from "@codemirror/theme-one-dark";
import { bracketMatching } from "@codemirror/language";
import { getLanguageExtension } from "../../utils/languageModes";
import { closeBrackets } from "@codemirror/autocomplete";
import type { HistoryEntry } from "../../hooks/useQueryHistory";
import TabBar from "./TabBar";
import type { Tab } from "./TabBar";
import styles from "./QueryEditor.module.css";

interface QueryEditorProps {
  language: string;
  onLanguageChange: (lang: string) => void;
  onExecute: (query: string) => void;
  onSave?: (query: string) => void;
  isLoading: boolean;
  onHistoryUp?: () => HistoryEntry | null;
  onHistoryDown?: () => HistoryEntry | null;
  onHistoryReset?: () => void;
  tabs: Tab[];
  activeTabId: string;
  onSelectTab: (id: string) => void;
  onAddTab: () => void;
  onCloseTab: (id: string) => void;
  onRenameTab: (id: string, name: string) => void;
  currentDatabase?: string;
  databaseType?: string;
}

export interface QueryEditorHandle {
  setContent: (text: string) => void;
  getContent: () => string;
}

const ALL_LANGUAGES = [
  { value: "gql", label: "GQL" },
  { value: "cypher", label: "Cypher" },
  { value: "graphql", label: "GraphQL" },
  { value: "gremlin", label: "Gremlin" },
  { value: "sparql", label: "SPARQL" },
];

const LPG_LANGUAGES = new Set(["gql", "cypher", "graphql", "gremlin"]);
const RDF_LANGUAGES = new Set(["gql", "sparql"]);

function languagesForType(dbType?: string) {
  if (dbType === "rdf" || dbType === "owl-schema" || dbType === "rdfs-schema") {
    return ALL_LANGUAGES.filter((l) => RDF_LANGUAGES.has(l.value));
  }
  return ALL_LANGUAGES.filter((l) => LPG_LANGUAGES.has(l.value));
}

function tabStorageKey(tabId: string): string {
  return `grafeo-editor-tab-${tabId}`;
}

const QueryEditor = forwardRef<QueryEditorHandle, QueryEditorProps>(
  function QueryEditor(
    {
      language,
      onLanguageChange,
      onExecute,
      onSave,
      isLoading,
      onHistoryUp,
      onHistoryDown,
      onHistoryReset,
      tabs,
      activeTabId,
      onSelectTab,
      onAddTab,
      onCloseTab,
      onRenameTab,
      currentDatabase,
      databaseType,
    },
    ref,
  ) {
    const editorRef = useRef<HTMLDivElement>(null);
    const viewRef = useRef<EditorView | null>(null);
    const langCompartment = useRef(new Compartment());
    const activeTabIdRef = useRef(activeTabId);
    activeTabIdRef.current = activeTabId;

    // Store callbacks in refs so CodeMirror keymap closures stay fresh
    const onExecuteRef = useRef(onExecute);
    const onHistoryUpRef = useRef(onHistoryUp);
    const onHistoryDownRef = useRef(onHistoryDown);
    const onHistoryResetRef = useRef(onHistoryReset);
    onExecuteRef.current = onExecute;
    onHistoryUpRef.current = onHistoryUp;
    onHistoryDownRef.current = onHistoryDown;
    onHistoryResetRef.current = onHistoryReset;

    // Expose setContent/getContent to parent via ref
    useImperativeHandle(ref, () => ({
      setContent(text: string) {
        const view = viewRef.current;
        if (view) {
          view.dispatch({
            changes: { from: 0, to: view.state.doc.length, insert: text },
          });
        }
      },
      getContent(): string {
        return viewRef.current?.state.doc.toString() ?? "";
      },
    }));

    useEffect(() => {
      if (!editorRef.current) return;

      // Restore persisted editor text or use default
      const saved = sessionStorage.getItem(tabStorageKey(activeTabId));
      const initialDoc = saved ?? "MATCH (n) RETURN n LIMIT 25";

      // Debounced persistence to sessionStorage (uses ref for current tab)
      let persistTimer: ReturnType<typeof setTimeout> | null = null;
      const persistListener = EditorView.updateListener.of(
        (update: ViewUpdate) => {
          if (update.docChanged) {
            if (persistTimer) clearTimeout(persistTimer);
            persistTimer = setTimeout(() => {
              sessionStorage.setItem(
                tabStorageKey(activeTabIdRef.current),
                update.state.doc.toString(),
              );
            }, 300);

            // Reset history cursor on manual edit
            onHistoryResetRef.current?.();
          }
        },
      );

      const state = EditorState.create({
        doc: initialDoc,
        extensions: [
          history(),
          bracketMatching(),
          closeBrackets(),
          langCompartment.current.of(getLanguageExtension(language)),
          oneDark,
          placeholder("Enter your query..."),
          persistListener,
          keymap.of([
            {
              key: "Ctrl-Enter",
              run: () => {
                const q = viewRef.current?.state.doc.toString() ?? "";
                if (q.trim()) onExecuteRef.current(q);
                return true;
              },
            },
            {
              key: "Cmd-Enter",
              run: () => {
                const q = viewRef.current?.state.doc.toString() ?? "";
                if (q.trim()) onExecuteRef.current(q);
                return true;
              },
            },
            {
              key: "Ctrl-ArrowUp",
              run: () => {
                const entry = onHistoryUpRef.current?.();
                if (entry && viewRef.current) {
                  viewRef.current.dispatch({
                    changes: {
                      from: 0,
                      to: viewRef.current.state.doc.length,
                      insert: entry.query,
                    },
                  });
                }
                return true;
              },
            },
            {
              key: "Ctrl-ArrowDown",
              run: () => {
                const entry = onHistoryDownRef.current?.();
                const view = viewRef.current;
                if (view) {
                  view.dispatch({
                    changes: {
                      from: 0,
                      to: view.state.doc.length,
                      insert: entry?.query ?? "",
                    },
                  });
                }
                return true;
              },
            },
            ...defaultKeymap,
            ...historyKeymap,
          ]),
          EditorView.theme({
            "&": {
              fontSize: "14px",
              fontFamily: "var(--font-mono)",
            },
            ".cm-content": {
              minHeight: "80px",
              padding: "8px 0",
            },
            ".cm-gutters": {
              background: "var(--bg-editor)",
              border: "none",
            },
            ".cm-scroller": {
              overflow: "auto",
            },
          }),
        ],
      });

      const view = new EditorView({
        state,
        parent: editorRef.current,
      });
      viewRef.current = view;

      return () => {
        if (persistTimer) clearTimeout(persistTimer);
        view.destroy();
      };
      // eslint-disable-next-line react-hooks/exhaustive-deps
    }, []);

    // Reconfigure language when dropdown changes
    useEffect(() => {
      const view = viewRef.current;
      if (!view) return;
      view.dispatch({
        effects: langCompartment.current.reconfigure(
          getLanguageExtension(language),
        ),
      });
    }, [language]);

    const handleRun = () => {
      const query = viewRef.current?.state.doc.toString() ?? "";
      if (query.trim()) onExecute(query);
    };

    const handleSave = () => {
      const query = viewRef.current?.state.doc.toString() ?? "";
      if (query.trim()) onSave?.(query);
    };

    return (
      <div className={styles.container}>
        <TabBar
          tabs={tabs}
          activeTabId={activeTabId}
          onSelectTab={onSelectTab}
          onAddTab={onAddTab}
          onCloseTab={onCloseTab}
          onRenameTab={onRenameTab}
        />
        <div className={styles.editor} ref={editorRef} />
        <div className={styles.toolbar}>
          <button
            className={styles.runButton}
            onClick={handleRun}
            disabled={isLoading}
          >
            {isLoading ? "Running..." : "Run"}
          </button>
          {onSave && (
            <button
              className={styles.saveButton}
              onClick={handleSave}
              title="Save query (Bookmark)"
            >
              Save
            </button>
          )}
          <select
            className={styles.languageSelect}
            value={language}
            onChange={(e) => onLanguageChange(e.target.value)}
          >
            {languagesForType(databaseType).map((l) => (
              <option key={l.value} value={l.value}>
                {l.label}
              </option>
            ))}
          </select>
          {currentDatabase && (
            <span className={styles.dbBadge} title={`Database: ${currentDatabase}`}>
              {currentDatabase}
            </span>
          )}
          <span className={styles.hint}>Ctrl+Enter to run</span>
        </div>
      </div>
    );
  },
);

export default QueryEditor;
