import { useState, useCallback, useRef } from "react";

export interface HistoryEntry {
  query: string;
  language: string;
  timestamp: number;
}

const HISTORY_KEY = "grafeo-query-history";
const MAX_HISTORY = 50;

function loadHistory(): HistoryEntry[] {
  try {
    const raw = localStorage.getItem(HISTORY_KEY);
    return raw ? JSON.parse(raw) : [];
  } catch {
    return [];
  }
}

function saveHistory(entries: HistoryEntry[]): void {
  localStorage.setItem(HISTORY_KEY, JSON.stringify(entries));
}

export function useQueryHistory() {
  const [entries, setEntries] = useState<HistoryEntry[]>(loadHistory);
  const cursorRef = useRef(-1);

  const add = useCallback((query: string, language: string) => {
    setEntries((prev) => {
      // Skip if identical to last entry
      if (prev.length > 0 && prev[0].query === query && prev[0].language === language) {
        return prev;
      }
      const entry: HistoryEntry = { query, language, timestamp: Date.now() };
      const next = [entry, ...prev].slice(0, MAX_HISTORY);
      saveHistory(next);
      return next;
    });
    cursorRef.current = -1;
  }, []);

  const navigateUp = useCallback((): HistoryEntry | null => {
    const list = loadHistory();
    if (list.length === 0) return null;
    const next = Math.min(cursorRef.current + 1, list.length - 1);
    cursorRef.current = next;
    return list[next];
  }, []);

  const navigateDown = useCallback((): HistoryEntry | null => {
    if (cursorRef.current <= 0) {
      cursorRef.current = -1;
      return null; // back to current (empty) state
    }
    cursorRef.current -= 1;
    const list = loadHistory();
    return list[cursorRef.current];
  }, []);

  const resetCursor = useCallback(() => {
    cursorRef.current = -1;
  }, []);

  return { entries, add, navigateUp, navigateDown, resetCursor };
}
