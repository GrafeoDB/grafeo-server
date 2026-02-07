import { useState, useRef, useEffect } from "react";
import styles from "./TabBar.module.css";

export interface Tab {
  id: string;
  name: string;
  language: string;
}

interface TabBarProps {
  tabs: Tab[];
  activeTabId: string;
  onSelectTab: (id: string) => void;
  onAddTab: () => void;
  onCloseTab: (id: string) => void;
  onRenameTab: (id: string, name: string) => void;
}

export default function TabBar({
  tabs,
  activeTabId,
  onSelectTab,
  onAddTab,
  onCloseTab,
  onRenameTab,
}: TabBarProps) {
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (renamingId && inputRef.current) {
      inputRef.current.focus();
      inputRef.current.select();
    }
  }, [renamingId]);

  const commitRename = () => {
    if (renamingId && renameValue.trim()) {
      onRenameTab(renamingId, renameValue.trim());
    }
    setRenamingId(null);
  };

  return (
    <div className={styles.container}>
      {tabs.map((tab) => (
        <div
          key={tab.id}
          className={`${styles.tab} ${tab.id === activeTabId ? styles.active : ""}`}
          onClick={() => onSelectTab(tab.id)}
          onDoubleClick={() => {
            setRenamingId(tab.id);
            setRenameValue(tab.name);
          }}
        >
          {renamingId === tab.id ? (
            <input
              ref={inputRef}
              className={styles.renameInput}
              value={renameValue}
              onChange={(e) => setRenameValue(e.target.value)}
              onBlur={commitRename}
              onKeyDown={(e) => {
                if (e.key === "Enter") commitRename();
                if (e.key === "Escape") setRenamingId(null);
              }}
              onClick={(e) => e.stopPropagation()}
            />
          ) : (
            <span className={styles.tabName}>{tab.name}</span>
          )}
          {tabs.length > 1 && (
            <button
              className={styles.closeButton}
              onClick={(e) => {
                e.stopPropagation();
                onCloseTab(tab.id);
              }}
              title="Close tab"
            >
              x
            </button>
          )}
        </div>
      ))}
      <button className={styles.addButton} onClick={onAddTab} title="New tab">
        +
      </button>
    </div>
  );
}
