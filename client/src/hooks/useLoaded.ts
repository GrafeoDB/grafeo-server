import { useCallback, useEffect, useRef, useState } from "react";
import { refusedKind } from "../api/client";
import type { Loaded } from "../context/ServerContext";

/** The outcome of a request, as a `Loaded`. */
export function settle<T>(promise: Promise<T>, set: (value: Loaded<T>) => void): void {
  promise
    .then((data) => set({ state: "ready", data }))
    .catch((err: unknown) => {
      const refused = refusedKind(err);
      set(
        refused
          ? { state: "refused", refused }
          : { state: "failed", message: err instanceof Error ? err.message : String(err) },
      );
    });
}

/**
 * A request's state for a page: loading, its answer, the refusal (401/403) or the failure. It asks again when a
 * value in `deps` changes (then it is loading) and on `reload` (then the old answer stays until the new one).
 */
export function useLoaded<T>(load: () => Promise<T>, deps: unknown[]): [Loaded<T>, () => void] {
  const key = JSON.stringify(deps);
  const loadRef = useRef(load);
  useEffect(() => {
    loadRef.current = load;
  });
  const [tick, setTick] = useState(0);
  const [answer, setAnswer] = useState<{ key: string; value: Loaded<T> } | null>(null);
  useEffect(() => {
    let cancelled = false;
    settle(loadRef.current(), (value) => {
      if (!cancelled) setAnswer({ key, value });
    });
    return () => {
      cancelled = true;
    };
  }, [key, tick]);
  const reload = useCallback(() => setTick((t) => t + 1), []);
  return [answer && answer.key === key ? answer.value : { state: "loading" }, reload];
}
