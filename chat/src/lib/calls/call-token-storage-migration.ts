const purgedLegacyPrefixes = new Set<string>();

export function purgeLegacyStoragePrefixFromLocalStorage(prefix: string): void {
  if (purgedLegacyPrefixes.has(prefix)) return;
  const storage = legacyLocalStorage();
  if (!storage) return;
  try {
    const keys: string[] = [];
    for (let index = 0; index < storage.length; index += 1) {
      const key = storage.key(index);
      if (key?.startsWith(`${prefix}.`)) keys.push(key);
    }
    for (const key of keys) storage.removeItem(key);
    purgedLegacyPrefixes.add(prefix);
  } catch {
    // Keep the prefix eligible for a later retry when storage access recovers.
  }
}

export function localStorageForLegacyPurge(): Storage | null {
  return legacyLocalStorage();
}

export function resetLegacyCallStorageMigrationForTests(): void {
  purgedLegacyPrefixes.clear();
}

function legacyLocalStorage(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage ?? null;
  } catch {
    return null;
  }
}
