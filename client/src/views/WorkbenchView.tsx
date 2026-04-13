import { useApp } from "../context/AppContext";
import { useWorkbenchRequired } from "../context/WorkbenchContext";
import QueryEditor from "../components/QueryEditor/QueryEditor";
import ResultsPanel from "../components/ResultsPanel/ResultsPanel";
import StatusBar from "../components/StatusBar/StatusBar";
import ShortcutHelp from "../components/ShortcutHelp/ShortcutHelp";

export default function WorkbenchView() {
  const { currentDatabase, databaseType } = useApp();
  const {
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
    onSaveQuery,
    historyNavigateUp,
    historyNavigateDown,
    historyResetCursor,
    viewMode,
    setViewMode,
    showHelp,
    toggleHelp,
  } = useWorkbenchRequired();

  return (
    <>
      <QueryEditor
        ref={editorRef}
        language={activeLanguage}
        onLanguageChange={onLanguageChange}
        onExecute={onExecute}
        onSave={onSaveQuery}
        isLoading={isLoading}
        onHistoryUp={historyNavigateUp}
        onHistoryDown={historyNavigateDown}
        onHistoryReset={historyResetCursor}
        tabs={tabState.tabs}
        activeTabId={tabState.activeTabId}
        onSelectTab={onSelectTab}
        onAddTab={onAddTab}
        onCloseTab={onCloseTab}
        onRenameTab={onRenameTab}
        currentDatabase={currentDatabase}
        databaseType={databaseType}
      />
      <ResultsPanel
        result={result}
        viewMode={viewMode}
        onViewModeChange={setViewMode}
      />
      <StatusBar
        result={result}
        error={error}
        isLoading={isLoading}
        onShowShortcuts={toggleHelp}
      />
      {showHelp && <ShortcutHelp onClose={toggleHelp} />}
    </>
  );
}
