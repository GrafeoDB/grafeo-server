import { useState, type KeyboardEvent, type ReactNode } from "react";
import {
  Button,
  CopyResults,
  Dialog,
  GraphView,
  Inspector,
  JsonView,
  PlanView,
  QueryEditor,
  ResultsPanel,
  ResultsTable,
  StateNote,
  ViewSwitch,
  type QueryResult,
} from "@grafeo-db/ui";
import { useServer } from "../context/ServerContext";
import { useWorkbench } from "../context/WorkbenchContext";

/** The editor, with Ctrl+Up and Ctrl+Down stepping through the history. */
export function EditorArea() {
  const wb = useWorkbench();
  const { activeDatabase } = useServer();
  const [saving, setSaving] = useState(false);

  const onKeyDown = (e: KeyboardEvent) => {
    if (!(e.ctrlKey || e.metaKey) || e.altKey || e.shiftKey) return;
    if (e.key !== "ArrowUp" && e.key !== "ArrowDown") return;
    e.preventDefault();
    e.stopPropagation();
    wb.historyStep(e.key === "ArrowUp" ? 1 : -1);
  };

  return (
    <div className="st-editor" onKeyDownCapture={onKeyDown}>
      <QueryEditor
        value={wb.text}
        onChange={wb.setText}
        language={wb.language}
        languages={wb.languages}
        onLanguageChange={wb.setLanguage}
        onRun={wb.run}
        onExplain={wb.explain}
        onSave={() => setSaving(true)}
        running={wb.running}
        database={activeDatabase}
      />
      <SaveQueryDialog
        open={saving}
        onClose={() => setSaving(false)}
        onSave={(name) => {
          wb.saveQuery(name);
          setSaving(false);
        }}
      />
    </div>
  );
}

function SaveQueryDialog({ open, onClose, onSave }: { open: boolean; onClose(): void; onSave(name: string): void }) {
  const [name, setName] = useState("");
  const close = () => {
    setName("");
    onClose();
  };
  const save = () => {
    if (!name.trim()) return;
    onSave(name);
    setName("");
  };
  return (
    <Dialog
      open={open}
      onClose={close}
      title="Save query"
      footer={
        <>
          <Button onClick={close}>Cancel</Button>
          <Button variant="primary" disabled={!name.trim()} onClick={save}>
            Save query
          </Button>
        </>
      }
    >
      <form
        className="gu-field"
        onSubmit={(e) => {
          e.preventDefault();
          save();
        }}
      >
        <label htmlFor="st-save-name">Name</label>
        <input
          id="st-save-name"
          className="gu-input"
          placeholder="friends of Richard"
          value={name}
          maxLength={80}
          autoFocus
          onChange={(e) => setName(e.target.value)}
        />
        <span className="st-hint">Kept in this browser, under Saved queries.</span>
      </form>
    </Dialog>
  );
}

/** The JSON the views show and Copy JSON copies: the rows as the server sent them, not the graph shape. */
function serverJson(result: QueryResult, rawRows: unknown[][] | null) {
  return { columns: result.columns, rows: rawRows ?? result.rows };
}

/** The graph, or another view of the result, with the floating view switch. */
export function MainArea() {
  const wb = useWorkbench();
  const { result, running, mainView } = wb;
  let body: ReactNode;
  if (mainView === "graph" || !result || running) {
    body = (
      <GraphView
        result={result}
        running={running}
        error={wb.error}
        refused={wb.refused}
        selectedId={wb.selectedId}
        onSelect={wb.setSelectedId}
      />
    );
  } else if (mainView === "table") {
    // The results panel is hidden while the main area shows the table, so the copies come here
    body = (
      <>
        <div className="st-main-copy">
          <CopyResults result={result} json={serverJson(result, wb.rawRows)} />
        </div>
        <ResultsTable result={result} onSelect={wb.setSelectedId} />
      </>
    );
  } else if (mainView === "json") {
    body = <JsonView value={serverJson(result, wb.rawRows)} total={result.rows.length} />;
  } else {
    body = <PlanView plan={result.plan} />;
  }
  const shown = mainView === "graph" || !result || running ? "graph" : mainView;
  return (
    <div className={`st-main st-main-${shown}`}>
      {body}
      <ViewSwitch value={mainView} onChange={wb.setMainView} />
    </div>
  );
}

export function InspectorArea() {
  const wb = useWorkbench();
  return (
    <Inspector
      result={wb.running ? null : wb.result}
      selectedId={wb.selectedId}
      onSelect={wb.setSelectedId}
      refused={wb.refused}
    />
  );
}

export function ResultsArea() {
  const wb = useWorkbench();
  if (wb.running) return <StateNote kind="running" />;
  if (wb.refused) return <StateNote kind={wb.refused} />;
  if (wb.error) return <StateNote kind="error" message={wb.error} />;
  if (!wb.result) return <StateNote kind="empty" />;
  return (
    <ResultsPanel
      result={wb.result}
      collapsed={wb.resultsCollapsed}
      onCollapsedChange={wb.setResultsCollapsed}
      // The main area shows the table itself; the panel then keeps only its warnings line
      hidden={wb.mainView === "table"}
      json={serverJson(wb.result, wb.rawRows)}
      onSelect={wb.setSelectedId}
    />
  );
}
