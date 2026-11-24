export type ChatStatePreference = "send" | "suppress";

const CHAT_STATE_STORAGE_KEY = "waddle:chat-states";

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

function isChatStatePreference(value: string | null): value is ChatStatePreference {
  return value === "send" || value === "suppress";
}

export function readStoredChatStatePreference(
  source: StorageReader | null = storage(),
): ChatStatePreference {
  let value: string | null = null;
  try {
    value = source?.getItem(CHAT_STATE_STORAGE_KEY) ?? null;
  } catch {
    value = null;
  }
  return isChatStatePreference(value) ? value : "send";
}

export function writeStoredChatStatePreference(
  value: ChatStatePreference,
  target: StorageWriter | null = storage(),
): void {
  if (!target) return;
  try {
    if (value === "send") {
      target.removeItem(CHAT_STATE_STORAGE_KEY);
      return;
    }
    target.setItem(CHAT_STATE_STORAGE_KEY, value);
  } catch {
    // Storage can be disabled or quota-limited; runtime preference state remains in memory.
  }
}

export function chatStatesAreEnabled(value: ChatStatePreference): boolean {
  return value === "send";
}
