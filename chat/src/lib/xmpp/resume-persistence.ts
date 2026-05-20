/**
 * Persisted resume state across page reloads.
 *
 * Two kinds of state survive a full reload, both keyed per account:
 *
 *   * MAM catch-up cursors (`PersistedReconnectCatchup`) — the
 *     timestamp + optional XEP-0313 archive UID + dedupe seen-ids
 *     for every DM peer and MUC room the user has seen. Without
 *     this, a hard reload (cold start, mobile Safari eviction)
 *     loses every cursor and the next `session:started` returns
 *     `[]` from `ReconnectCatchup.onSessionStarted()` — so MAM
 *     catch-up does NOT run and missed messages are only
 *     re-fetched if the user manually scrolls back.
 *
 *   * XEP-0198 Stream Management resume state
 *     (`PersistedSmResumeState`) — the `previd` plus inbound /
 *     outbound stanza counts. The richer "handle" form from the
 *     WASM client is a live JS object that cannot be serialized;
 *     the POD `{previd, inboundH, outboundH}` triple can be, and
 *     the fallback path in `doConnect` already knows how to feed
 *     it back via `with_resume_state`.
 *
 * The shape mirrors `outbound-queue-store.ts` (same `waddle.chat.*`
 * prefix family, same per-account key namespacing, same defensive
 * read/write error handling around localStorage availability and
 * quota) so the storage surface stays uniform.
 */

import { reportError } from "@/lib/telemetry";

const CATCHUP_PREFIX = "waddle.chat.resume-cursors";
const SM_PREFIX = "waddle.chat.sm-resume";

export type PersistedSeenCursor = {
  timestamp: string;
  archiveId?: string;
  seenIds?: string[];
};

export type PersistedReconnectCatchup = {
  dmLastSeen: Array<[string, PersistedSeenCursor]>;
  roomLastSeen: Array<[string, PersistedSeenCursor]>;
};

/**
 * The on-wire shape that `doConnect` feeds to `with_resume_state`.
 * Stamped with a private `savedAt` internally so a stale POD can be
 * rejected without forcing every caller to know about TTL — see
 * `loadSm`.
 */
export type PersistedSmResumeState = {
  previd: string;
  inboundH: number;
  outboundH: number;
};

interface PersistedSmEnvelope extends PersistedSmResumeState {
  savedAt: number;
}

/**
 * Reject a persisted SM POD older than this window. Servers usually
 * GC the corresponding session much sooner (XEP-0198 §5); 24h is
 * generous but bounds the cost of a guaranteed `<failed/>` on every
 * cold start with a weeks-old POD.
 */
const SM_TTL_MS = 24 * 60 * 60 * 1000;

export interface ResumePersistence {
  loadCatchup(): PersistedReconnectCatchup | null;
  saveCatchup(snapshot: PersistedReconnectCatchup): void;
  clearCatchup(): void;
  loadSm(): PersistedSmResumeState | null;
  saveSm(state: PersistedSmResumeState): void;
  clearSm(): void;
}

/** No-op persistence — used in tests / non-browser contexts. */
export const nullResumePersistence: ResumePersistence = {
  loadCatchup: () => null,
  saveCatchup: () => undefined,
  clearCatchup: () => undefined,
  loadSm: () => null,
  saveSm: () => undefined,
  clearSm: () => undefined,
};

export function createLocalStorageResumePersistence(accountKey: string): ResumePersistence {
  const catchupKey = `${CATCHUP_PREFIX}.${accountKey}`;
  const smKey = `${SM_PREFIX}.${accountKey}`;

  return {
    loadCatchup() {
      return readJson<PersistedReconnectCatchup>(catchupKey, isPersistedReconnectCatchup, "catchup");
    },
    saveCatchup(snapshot) {
      writeJson(catchupKey, snapshot, "catchup");
    },
    clearCatchup() {
      removeKey(catchupKey, "catchup");
    },
    loadSm() {
      const envelope = readJson<PersistedSmEnvelope>(smKey, isPersistedSmEnvelope, "sm");
      if (!envelope) return null;
      // Drop entries past the TTL — the server has GC'd the
      // corresponding session, so feeding the POD back to
      // `with_resume_state` is a guaranteed `<failed/>`. Returning
      // null lets `doConnect` fall through to a fresh bind.
      if (Date.now() - envelope.savedAt > SM_TTL_MS) {
        removeKey(smKey, "sm");
        return null;
      }
      const { previd, inboundH, outboundH } = envelope;
      return { previd, inboundH, outboundH };
    },
    saveSm(state) {
      const envelope: PersistedSmEnvelope = { ...state, savedAt: Date.now() };
      writeJson(smKey, envelope, "sm");
    },
    clearSm() {
      removeKey(smKey, "sm");
    },
  };
}

function storage(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function readJson<T>(key: string, validate: (value: unknown) => value is T, kind: string): T | null {
  const s = storage();
  if (!s) return null;
  try {
    const raw = s.getItem(key);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    return validate(parsed) ? parsed : null;
  } catch (err) {
    reportError("storage.read", err, {
      recoverable: true,
      detail: `resume-persistence read failed (${kind})`,
      key,
    });
    return null;
  }
}

function writeJson(key: string, value: unknown, kind: string): void {
  const s = storage();
  if (!s) return;
  try {
    s.setItem(key, JSON.stringify(value));
  } catch (err) {
    const name = err instanceof Error ? err.name : "";
    const errorKind = name === "QuotaExceededError" ? "storage.quota" : "storage.write";
    reportError(errorKind, err, {
      recoverable: true,
      detail: `resume-persistence write failed (${kind})`,
      key,
    });
  }
}

function removeKey(key: string, kind: string): void {
  const s = storage();
  if (!s) return;
  try {
    s.removeItem(key);
  } catch (err) {
    reportError("storage.write", err, {
      recoverable: true,
      detail: `resume-persistence clear failed (${kind})`,
      key,
    });
  }
}

function isPersistedSeenCursor(value: unknown): value is PersistedSeenCursor {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  if (typeof candidate.timestamp !== "string") return false;
  if (candidate.archiveId !== undefined && typeof candidate.archiveId !== "string") return false;
  if (candidate.seenIds !== undefined) {
    if (!Array.isArray(candidate.seenIds)) return false;
    if (!candidate.seenIds.every((id) => typeof id === "string")) return false;
  }
  return true;
}

function isCursorMap(value: unknown): value is Array<[string, PersistedSeenCursor]> {
  if (!Array.isArray(value)) return false;
  return value.every((entry) =>
    Array.isArray(entry)
    && entry.length === 2
    && typeof entry[0] === "string"
    && isPersistedSeenCursor(entry[1]),
  );
}

function isPersistedReconnectCatchup(value: unknown): value is PersistedReconnectCatchup {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  return isCursorMap(candidate.dmLastSeen) && isCursorMap(candidate.roomLastSeen);
}

function isPersistedSmEnvelope(value: unknown): value is PersistedSmEnvelope {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  return (
    typeof candidate.previd === "string"
    && typeof candidate.inboundH === "number"
    && typeof candidate.outboundH === "number"
    && typeof candidate.savedAt === "number"
  );
}
