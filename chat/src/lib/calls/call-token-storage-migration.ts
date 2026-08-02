import { reportError } from "@/lib/telemetry";

const purgedLegacyPrefixes = new Set<string>();

export function purgeLegacyStoragePrefixFromLocalStorage(prefix: string): void {
  if (purgedLegacyPrefixes.has(prefix)) return;
  const storage = legacyLocalStorage();
  if (!storage) return;
  try {
    for (const key of keysWithPrefix(storage, prefix)) storage.removeItem(key);
    purgedLegacyPrefixes.add(prefix);
  } catch {
    // Keep the prefix eligible for a later retry when storage access recovers.
  }
}

export function localStorageForLegacyPurge(): Storage | null {
  return legacyLocalStorage();
}

/**
 * The call caches' storage: session-scoped (bearer tokens must not live in
 * origin-wide persistent storage, #1450), with the legacy localStorage purge
 * repeated here so SSR-first imports retry once hydration makes browser
 * storage available.
 */
export function callCacheSessionStorage(cachePrefix: string): Storage | null {
  purgeLegacyStoragePrefixFromLocalStorage(cachePrefix);
  if (typeof window === "undefined") return null;
  try {
    return window.sessionStorage ?? null;
  } catch {
    return null;
  }
}

export function clearCallCacheKeysWithPrefix(
  storage: Storage | null,
  prefix: string,
  detail: string,
): void {
  if (!storage) return;
  try {
    for (const key of keysWithPrefix(storage, prefix)) storage.removeItem(key);
  } catch (err) {
    reportError("storage.write", err, { recoverable: true, detail });
  }
}

export function resetLegacyCallStorageMigrationForTests(): void {
  purgedLegacyPrefixes.clear();
}

function keysWithPrefix(storage: Storage, prefix: string): string[] {
  const keys: string[] = [];
  for (let index = 0; index < storage.length; index += 1) {
    const key = storage.key(index);
    if (key?.startsWith(`${prefix}.`)) keys.push(key);
  }
  return keys;
}

function legacyLocalStorage(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage ?? null;
  } catch {
    return null;
  }
}
