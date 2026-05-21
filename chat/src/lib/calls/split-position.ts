import { map } from "nanostores";
import { normalizeMucCallRoomJid } from "./muc-call-presence";

/**
 * Lower / upper bounds for the call/chat split, expressed as the
 * percentage of the parent flex column the call region occupies.
 *
 * Below 25% the call grid becomes too cramped to recognise faces;
 * above 75% the message lane stops being usable as a chat surface
 * — both extremes defeat the whole point of the split layout.
 */
export const SPLIT_MIN_PERCENT = 25;
export const SPLIT_MAX_PERCENT = 75;
export const SPLIT_DEFAULT_PERCENT = 50;

const STORAGE_KEY = "waddle:call-split-positions";

/**
 * Per-room map of saved split positions. Keyed by the bare room JID
 * (normalized via `normalizeMucCallRoomJid`) so a user who picks a
 * chat-heavy 30% split in `#design` still gets a clean 50% split in
 * `#general` next time they call there.
 */
type SplitPositions = Record<string, number>;

function clamp(value: number): number {
  if (!Number.isFinite(value)) return SPLIT_DEFAULT_PERCENT;
  if (value < SPLIT_MIN_PERCENT) return SPLIT_MIN_PERCENT;
  if (value > SPLIT_MAX_PERCENT) return SPLIT_MAX_PERCENT;
  return value;
}

function readInitialPositions(): SplitPositions {
  if (typeof window === "undefined") return {};
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return {};
    const out: SplitPositions = {};
    for (const [key, value] of Object.entries(parsed as Record<string, unknown>)) {
      if (typeof value === "number" && Number.isFinite(value)) {
        out[key] = clamp(value);
      }
    }
    return out;
  } catch {
    return {};
  }
}

export const $callSplitPositions = map<SplitPositions>(readInitialPositions());

let persistTimer: ReturnType<typeof setTimeout> | null = null;
const PERSIST_DEBOUNCE_MS = 200;

$callSplitPositions.subscribe((value) => {
  if (typeof window === "undefined") return;
  if (persistTimer) clearTimeout(persistTimer);
  persistTimer = setTimeout(() => {
    persistTimer = null;
    try {
      window.localStorage.setItem(STORAGE_KEY, JSON.stringify(value));
    } catch {
      // Best-effort.
    }
  }, PERSIST_DEBOUNCE_MS);
});

/** Read the saved split for a room, or the default when none exists. */
export function getSplitPercent(roomJid: string): number {
  const key = normalizeMucCallRoomJid(roomJid);
  if (!key) return SPLIT_DEFAULT_PERCENT;
  const stored = $callSplitPositions.get()[key];
  return clamp(stored ?? SPLIT_DEFAULT_PERCENT);
}

/** Write a new (clamped) split for a room. */
export function setSplitPercent(roomJid: string, percent: number): void {
  const key = normalizeMucCallRoomJid(roomJid);
  if (!key) return;
  $callSplitPositions.setKey(key, clamp(percent));
}

/** Test-only: reset the store and clear any pending persist tick. */
export function resetSplitPositionsForTests(): void {
  if (persistTimer) {
    clearTimeout(persistTimer);
    persistTimer = null;
  }
  $callSplitPositions.set({});
  if (typeof window !== "undefined") {
    try {
      window.localStorage.removeItem(STORAGE_KEY);
    } catch {
      // ignore
    }
  }
}
