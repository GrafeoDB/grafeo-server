/**
 * Browser storage that never throws: a private window or blocked site data
 * makes `localStorage` throw, and the Studio then works for the session only.
 */
export function readStored(key: string, storage: "local" | "session" = "local"): string | null {
  try {
    return (storage === "local" ? localStorage : sessionStorage).getItem(key);
  } catch {
    return null;
  }
}

export function writeStored(key: string, value: string, storage: "local" | "session" = "local"): void {
  try {
    (storage === "local" ? localStorage : sessionStorage).setItem(key, value);
  } catch {
    // Storage blocked: the value lives for this session only.
  }
}

export function removeStored(key: string, storage: "local" | "session" = "local"): void {
  try {
    (storage === "local" ? localStorage : sessionStorage).removeItem(key);
  } catch {
    // Storage blocked: nothing was stored.
  }
}

/** JSON from storage, or `fallback` when it is missing, unreadable or fails `valid`. */
export function readJson<T>(key: string, fallback: T, valid: (value: unknown) => value is T): T {
  const raw = readStored(key);
  if (!raw) return fallback;
  try {
    const value: unknown = JSON.parse(raw);
    return valid(value) ? value : fallback;
  } catch {
    return fallback;
  }
}

export function writeJson(key: string, value: unknown): void {
  writeStored(key, JSON.stringify(value));
}
