export type ReadReceiptPreference = "send" | "suppress";

const READ_RECEIPT_STORAGE_KEY = "waddle:read-receipts";

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

function isReadReceiptPreference(value: string | null): value is ReadReceiptPreference {
  return value === "send" || value === "suppress";
}

export function readStoredReadReceiptPreference(
  source: StorageReader | null = storage(),
): ReadReceiptPreference {
  let value: string | null = null;
  try {
    value = source?.getItem(READ_RECEIPT_STORAGE_KEY) ?? null;
  } catch {
    value = null;
  }
  return isReadReceiptPreference(value) ? value : "send";
}

export function writeStoredReadReceiptPreference(
  value: ReadReceiptPreference,
  target: StorageWriter | null = storage(),
): void {
  if (!target) return;
  try {
    if (value === "send") {
      target.removeItem(READ_RECEIPT_STORAGE_KEY);
      return;
    }
    target.setItem(READ_RECEIPT_STORAGE_KEY, value);
  } catch {
    // Storage can be disabled or quota-limited; runtime preference state remains in memory.
  }
}

export function readReceiptsAreEnabled(value: ReadReceiptPreference): boolean {
  return value === "send";
}
