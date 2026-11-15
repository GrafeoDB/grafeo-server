import { Outlet, useLocation, useNavigate } from "react-router-dom";
import {
  AppShell,
  QueryTabs,
  ShortcutHelp,
  StatusBar,
  StatusItem,
  TopBar,
  FORBIDDEN_NOTE,
  UNAUTHENTICATED_NOTE,
  type Shortcut,
} from "@grafeo-db/ui";
import { useServer } from "../context/ServerContext";
import { useWorkbench } from "../context/WorkbenchContext";
import StudioSidebar from "./StudioSidebar";
import { EditorArea, InspectorArea, MainArea, ResultsArea } from "./Workbench";
import { languageName, modelName, resultLine } from "./format";

const SHORTCUTS: Shortcut[] = [
  { keys: ["Ctrl", "Enter"], action: "Run the query" },
  { keys: ["Ctrl", "↑"], action: "Previous query from the history" },
  { keys: ["Ctrl", "↓"], action: "Next query from the history" },
  { keys: ["←", "→"], action: "Move between query tabs" },
  { keys: ["Delete"], action: "Close the focused query tab" },
  { keys: ["?"], action: "Show keyboard shortcuts" },
];

/** The refusal in a few words, for the status bar. */
const REFUSED_SHORT = {
  unauthenticated: UNAUTHENTICATED_NOTE.split(".")[0],
  forbidden: FORBIDDEN_NOTE.split(".")[0],
};

function StudioStatus() {
  const { reachable, activeModel } = useServer();
  const wb = useWorkbench();
  let state;
  if (reachable === false) state = <StatusItem tone="error">Server unreachable</StatusItem>;
  else if (wb.running) state = <StatusItem>Running query…</StatusItem>;
  else if (wb.refused) state = <StatusItem tone="error">{REFUSED_SHORT[wb.refused]}</StatusItem>;
  else if (wb.error)
    state = (
      <StatusItem tone="error" title={wb.error}>
        {wb.error}
      </StatusItem>
    );
  else state = <StatusItem tone="ok">Ready</StatusItem>;
  return (
    <StatusBar
      start={
        <>
          {state}
          <StatusItem>{languageName(wb.language)}</StatusItem>
          <StatusItem>{modelName(activeModel)}</StatusItem>
        </>
      }
      end={
        <>
          {wb.result && !wb.running ? <StatusItem>{resultLine(wb.result)}</StatusItem> : null}
          <StatusItem>{window.location.host}</StatusItem>
        </>
      }
    />
  );
}

/**
 * The Studio's frame: the top bar, sidebar and status bar stay, and the work area is the workbench at `/` or a page
 * on every other route, with the workbench kept behind it.
 */
export default function StudioShell() {
  const { pathname } = useLocation();
  const navigate = useNavigate();
  const { health } = useServer();
  const wb = useWorkbench();
  const onWorkbench = pathname === "/";

  const top = (
    <TopBar
      appName="Grafeo Studio"
      version={health?.version}
      tabs={
        <QueryTabs
          tabs={wb.tabs}
          activeId={onWorkbench ? wb.activeTab.id : null}
          onSelect={(id) => {
            wb.selectTab(id);
            if (!onWorkbench) navigate("/");
          }}
          onClose={wb.closeTab}
          onRename={wb.renameTab}
          onAdd={() => {
            wb.addTab();
            if (!onWorkbench) navigate("/");
          }}
        />
      }
      help={<ShortcutHelp shortcuts={SHORTCUTS} />}
    />
  );

  // The workbench areas are always given: while a page shows they stay mounted and hidden, so the graph keeps its
  // layout, zoom and selection, and the editor its text and undo history.
  return (
    <AppShell
      variant="workbench"
      top={top}
      sidebar={<StudioSidebar />}
      page={onWorkbench ? undefined : <Outlet />}
      editor={<EditorArea />}
      main={<MainArea />}
      inspector={<InspectorArea />}
      inspectorOpen={wb.selectedId !== null && wb.result !== null && !wb.running}
      onInspectorClose={() => wb.setSelectedId(null)}
      results={<ResultsArea />}
      status={<StudioStatus />}
    />
  );
}
