import { reportError } from "@/lib/telemetry";

/**
 * Runs on EVERY cache access, not once: during a rolling deployment an
 * already-open pre-migration tab keeps writing bearer tokens into the
 * shared origin-wide localStorage after this tab's first purge, so a
 * one-shot guard would leave the recreated token persisted (#1621
 * review round 5). The scan is a handful of keys on rare call-cache
 * touches.
 */
export function purgeLegacyStoragePrefixFromLocalStorage(prefix: string): void {
  const storage = legacyLocalStorage();
  if (!storage) return;
  try {
    for (const key of keysWithPrefix(storage, prefix)) storage.removeItem(key);
  } catch {
    // Retried on the next cache access when storage access recovers.
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
