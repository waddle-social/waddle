// XEP-0392 §"Business Rules": "Implementations SHOULD allow the user to
// turn off any colorization completely." When off, avatars keep their
// static theme tint instead of a per-identity colour.

export type ConsistentColorPreference = "on" | "off";

const CONSISTENT_COLOR_STORAGE_KEY = "waddle:consistent-color";

type StorageReader = Pick<Storage, "getItem">;
type StorageWriter = Pick<Storage, "removeItem" | "setItem">;

function storage(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export function readStoredConsistentColorPreference(
  source: StorageReader | null = storage(),
): ConsistentColorPreference {
  try {
    return source?.getItem(CONSISTENT_COLOR_STORAGE_KEY) === "off" ? "off" : "on";
  } catch {
    return "on";
  }
}

export function writeStoredConsistentColorPreference(
  value: ConsistentColorPreference,
  target: StorageWriter | null = storage(),
): void {
  if (!target) return;
  try {
    if (value === "on") {
      target.removeItem(CONSISTENT_COLOR_STORAGE_KEY);
      return;
    }
    target.setItem(CONSISTENT_COLOR_STORAGE_KEY, value);
  } catch {
    // Storage can be disabled or quota-limited; runtime preference state remains in memory.
  }
}
