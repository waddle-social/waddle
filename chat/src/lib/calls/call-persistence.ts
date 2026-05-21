import { atom } from "nanostores";

/**
 * sessionStorage-backed stub of "the last call this tab was in". On
 * a hard refresh (F5) the in-memory `$callState` is wiped along with
 * the XMPP WebSocket, the Jingle session, and the LiveKit token, so
 * the user is XMPP-wise no longer in the call. The stub lets the
 * UI surface a "rejoin in #room" affordance after the room's Muji
 * presence repopulates, instead of silently dropping the user.
 *
 * Lifecycle:
 * - Written when `$callState` enters `phase: "active"` with
 *   `kind: "muc"` (only group calls qualify — DM calls have a
 *   different rejoin shape and aren't covered here).
 * - Cleared on local hangup, on `phase: "ended"`, or when the
 *   room's Muji participant list drops to zero (call wrapped up
 *   while the tab was gone).
 * - Bounded by tab lifetime (sessionStorage), which matches the
 *   "active call session" intent — closing the tab is a deliberate
 *   exit, no need to nag the user about an old call on reopen.
 *
 * Stored shape is intentionally minimal — the room JID is the only
 * field a rejoin needs, and the timestamp lets us age-out stubs
 * that survived past their welcome.
 */
type PersistedCallStub = {
  roomJid: string;
  /** Wall-clock ms when the call became active in this tab. Used
   *  to age out stubs older than `STALE_AFTER_MS` on boot. */
  joinedAt: number;
};

const STORAGE_KEY = "waddle:active-muc-call";
const STALE_AFTER_MS = 12 * 60 * 60 * 1000;

function readFromStorage(): PersistedCallStub | null {
  if (typeof window === "undefined") return null;
  try {
    const raw = window.sessionStorage.getItem(STORAGE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<PersistedCallStub> | null;
    if (!parsed || typeof parsed.roomJid !== "string" || !parsed.roomJid) {
      return null;
    }
    const joinedAt = typeof parsed.joinedAt === "number" ? parsed.joinedAt : 0;
    if (Date.now() - joinedAt > STALE_AFTER_MS) {
      window.sessionStorage.removeItem(STORAGE_KEY);
      return null;
    }
    return { roomJid: parsed.roomJid, joinedAt };
  } catch {
    return null;
  }
}

function writeToStorage(stub: PersistedCallStub | null): void {
  if (typeof window === "undefined") return;
  try {
    if (stub === null) {
      window.sessionStorage.removeItem(STORAGE_KEY);
      return;
    }
    window.sessionStorage.setItem(STORAGE_KEY, JSON.stringify(stub));
  } catch {
    // sessionStorage may throw in private modes or when quota is
    // exhausted — the in-memory atom still reflects the intended
    // state, so the UI keeps working for the current tab.
  }
}

export const $persistedCallStub = atom<PersistedCallStub | null>(readFromStorage());

export function persistCallStub(roomJid: string): void {
  const stub: PersistedCallStub = { roomJid, joinedAt: Date.now() };
  $persistedCallStub.set(stub);
  writeToStorage(stub);
}

export function clearPersistedCallStub(): void {
  $persistedCallStub.set(null);
  writeToStorage(null);
}

/** Test-only reset hook: wipes both the in-memory atom and the
 *  storage entry so test ordering doesn't leak stale stubs. */
export function __resetPersistedCallStubForTests(): void {
  $persistedCallStub.set(null);
  if (typeof window === "undefined") return;
  try {
    window.sessionStorage.removeItem(STORAGE_KEY);
  } catch {
    // ignore
  }
}
