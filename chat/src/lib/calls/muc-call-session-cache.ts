import { barePeerJid } from "@/lib/xmpp/jid";
import { normalizeMucCallRoomJid } from "./muc-call-presence";
import { reportError } from "@/lib/telemetry";
import { map } from "nanostores";
import type { CallMedia, LiveKitJoin } from "./types";
import {
  callCacheSessionStorage,
  clearCallCacheKeysWithPrefix,
  localStorageForLegacyPurge,
  purgeLegacyStoragePrefixFromLocalStorage,
} from "./call-token-storage-migration";

const CACHE_PREFIX = "waddle.chat.muc-call-sessions";
const CACHE_WINDOW_MS = 24 * 60 * 60 * 1000;

// Remove bearer tokens written by pre-#1450 builds as soon as this cache is
// initialized. `storage()` repeats the one-shot call so SSR-first imports can
// retry after hydration makes browser storage available.
purgeLegacyStoragePrefixFromLocalStorage(CACHE_PREFIX);
const MAX_CACHE_ENTRIES = 64;

type CachedMucCallSession = {
  roomJid: string;
  sid: string;
  selfFullJid: string;
  updatedAt: string;
  media?: CallMedia;
  /**
   * LiveKit join credentials minted by the SFU during the original
   * `beginMucCall` flow. Persisting them is what makes the resume
   * path (`resumeMucCallActivity`) able to reconnect to the same
   * LiveKit room without going through a fresh Jingle session-initiate
   * handshake — LK identity-uniqueness then displaces any orphan
   * session left from the dead pre-reload resource.
   *
   * Optional because retained-but-pending teardown entries (from
   * `markMucCallSessionTerminatePending`) don't need fresh join
   * credentials — they only carry the sid for the cleanup terminate.
   */
  join?: LiveKitJoin;
  terminatePending?: boolean;
};

export const $mucCallTerminatePendingSessions = map<Record<string, CachedMucCallSession>>({});

export function rememberMucCallSession(options: {
  roomJid: string;
  sid: string;
  selfFullJid?: string | null;
  media?: CallMedia | null;
  join?: LiveKitJoin | null;
  now?: Date;
}): void {
  const entry = normalizeEntry({
    roomJid: options.roomJid,
    sid: options.sid,
    selfFullJid: options.selfFullJid?.trim() ?? "",
    ...(options.media ? { media: options.media } : {}),
    ...(options.join ? { join: options.join } : {}),
    updatedAt: options.now?.toISOString() ?? new Date().toISOString(),
  });
  if (!entry) return;
  forgetPendingEntriesForRoom(entry);
  const selfBare = normalizedBare(entry.selfFullJid);
  if (!selfBare) return;
  const now = options.now ?? new Date();
  const entries = readEntries(selfBare)
    .filter((candidate) => !isExpired(candidate.updatedAt, now))
    .filter((candidate) =>
      !candidate.terminatePending ||
      candidate.roomJid !== entry.roomJid ||
      candidate.selfFullJid !== entry.selfFullJid
    )
    .filter((candidate) =>
      candidate.roomJid !== entry.roomJid ||
      candidate.sid !== entry.sid ||
      candidate.selfFullJid !== entry.selfFullJid
    );
  entries.unshift(entry);
  writeEntries(selfBare, entries.slice(0, MAX_CACHE_ENTRIES));
}

export function readMucCallSession(options: {
  roomJid: string;
  selfFullJid?: string | null;
  now?: Date;
}): CachedMucCallSession | null {
  const roomJid = normalizeMucCallRoomJid(options.roomJid);
  const selfFullJid = options.selfFullJid?.trim() ?? "";
  const selfBare = normalizedBare(selfFullJid);
  if (!roomJid || !selfFullJid || !selfBare) return null;

  const now = options.now ?? new Date();
  let changed = false;
  const freshEntries = readEntries(selfBare).filter((entry) => {
    const keep = !isExpired(entry.updatedAt, now);
    changed ||= !keep;
    return keep;
  });
  if (changed) writeEntries(selfBare, freshEntries);

  const entry = freshEntries.find((entry) =>
    entry.roomJid === roomJid &&
    entry.selfFullJid === selfFullJid
  ) ?? null;
  syncPendingEntries(freshEntries.filter((candidate) => candidate.terminatePending));
  return entry;
}

export function markMucCallSessionTerminatePending(options: {
  roomJid: string;
  sid: string;
  selfFullJid?: string | null;
  media?: CallMedia | null;
  now?: Date;
}): void {
  const entry = normalizeEntry({
    roomJid: options.roomJid,
    sid: options.sid,
    selfFullJid: options.selfFullJid?.trim() ?? "",
    ...(options.media ? { media: options.media } : {}),
    updatedAt: options.now?.toISOString() ?? new Date().toISOString(),
    terminatePending: true,
  });
  if (!entry) return;
  const selfBare = normalizedBare(entry.selfFullJid);
  if (!selfBare) return;
  const now = options.now ?? new Date();
  const entries = readEntries(selfBare)
    .filter((candidate) => !isExpired(candidate.updatedAt, now))
    .map((candidate) => (
      candidate.roomJid === entry.roomJid &&
      candidate.sid === entry.sid &&
      candidate.selfFullJid === entry.selfFullJid
        ? {
            ...candidate,
            ...(entry.media ? { media: entry.media } : {}),
            terminatePending: true,
            updatedAt: entry.updatedAt,
          }
        : candidate
    ));
  if (!entries.some((candidate) =>
    candidate.roomJid === entry.roomJid &&
    candidate.sid === entry.sid &&
    candidate.selfFullJid === entry.selfFullJid
  )) {
    entries.unshift(entry);
  }
  writeEntries(selfBare, entries.slice(0, MAX_CACHE_ENTRIES));
  syncPendingEntries(entries.filter((candidate) => candidate.terminatePending));
}

/**
 * Remove a terminate-pending mark without forgetting the cached session. A
 * SUPERSEDED tab must not durably terminate a call its successor tab now
 * owns: the mark would make the successor's reload reject the still-running
 * call in `canResumeMucCallActivity` and queue a mixer terminate against it.
 */
export function unmarkMucCallSessionTerminatePending(options: {
  roomJid: string;
  sid: string;
  selfFullJid?: string | null;
}): void {
  const roomJid = normalizeMucCallRoomJid(options.roomJid);
  const selfFullJid = options.selfFullJid?.trim() ?? "";
  const selfBare = normalizedBare(selfFullJid);
  if (!roomJid || !selfFullJid || !selfBare) return;
  const entries = readEntries(selfBare).map((candidate): CachedMucCallSession => {
    if (
      candidate.roomJid !== roomJid ||
      candidate.sid !== options.sid ||
      candidate.selfFullJid !== selfFullJid ||
      !candidate.terminatePending
    ) return candidate;
    const { terminatePending: _dropped, ...rest } = candidate;
    return rest;
  });
  writeEntries(selfBare, entries);
  syncPendingEntries(entries.filter((entry) => entry.terminatePending));
}

export function forgetMucCallSession(options: {
  roomJid: string;
  selfFullJid?: string | null;
  sid?: string | null;
}): void {
  const roomJid = normalizeMucCallRoomJid(options.roomJid);
  const selfFullJid = options.selfFullJid?.trim() ?? "";
  const selfBare = normalizedBare(selfFullJid);
  if (!roomJid || !selfFullJid || !selfBare) return;
  const entries = readEntries(selfBare);
  const next = entries.filter((entry) =>
    entry.roomJid !== roomJid ||
    entry.selfFullJid !== selfFullJid ||
    (options.sid ? entry.sid !== options.sid : false)
  );
  if (next.length !== entries.length) writeEntries(selfBare, next);
  syncPendingEntries(next.filter((entry) => entry.terminatePending));
}

export function clearMucCallSessionCacheForAccount(selfBareJid: string): void {
  const selfBare = normalizedBare(selfBareJid);
  if (!selfBare) return;
  removeKey(cacheKey(selfBare));
  const current = $mucCallTerminatePendingSessions.get();
  const next = Object.fromEntries(
    Object.entries(current).filter(([, entry]) => normalizedBare(entry.selfFullJid) !== selfBare),
  );
  if (Object.keys(next).length !== Object.keys(current).length) {
    $mucCallTerminatePendingSessions.set(next);
  }
}

export function clearAllMucCallSessionCacheForTests(): void {
  $mucCallTerminatePendingSessions.set({});
  clearCallCacheKeysWithPrefix(storage(), CACHE_PREFIX, "muc call session cache clear failed");
  clearCallCacheKeysWithPrefix(
    localStorageForLegacyPurge(),
    CACHE_PREFIX,
    "muc call session cache clear failed",
  );
}

function normalizeEntry(entry: CachedMucCallSession): CachedMucCallSession | null {
  const roomJid = normalizeMucCallRoomJid(entry.roomJid);
  const selfFullJid = entry.selfFullJid.trim();
  const sid = entry.sid.trim();
  if (!roomJid || !selfFullJid || !sid) return null;
  if (!normalizedBare(selfFullJid)) return null;
  if (Number.isNaN(Date.parse(entry.updatedAt))) return null;
  // Discard a malformed join blob silently rather than dropping the
  // whole entry — the resume path falls back to a fresh
  // `beginMucCall` if `join` is missing, so a corrupted cached blob
  // degrades cleanly instead of breaking the retained-leave path.
  const validJoin = entry.join && isLiveKitJoin(entry.join) && entry.join.identity === selfFullJid
    ? entry.join
    : undefined;
  return {
    roomJid,
    sid,
    selfFullJid,
    updatedAt: entry.updatedAt,
    ...(isCallMedia(entry.media) ? { media: entry.media } : {}),
    ...(validJoin ? { join: validJoin } : {}),
    ...(entry.terminatePending ? { terminatePending: true } : {}),
  };
}

function readEntries(selfBareJid: string): CachedMucCallSession[] {
  const s = storage();
  if (!s) return [];
  const key = cacheKey(selfBareJid);
  try {
    const raw = s.getItem(key);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed
      .map((entry) => isCachedMucCallSession(entry) ? normalizeEntry(entry) : null)
      .filter((entry): entry is CachedMucCallSession => !!entry);
  } catch (err) {
    void err;
    reportError({ kind: "storage.read", reason: "read-failed", area: "muc-call-session-cache" });
    return [];
  }
}

function writeEntries(selfBareJid: string, entries: CachedMucCallSession[]): void {
  const s = storage();
  if (!s) return;
  const key = cacheKey(selfBareJid);
  try {
    if (entries.length === 0) {
      s.removeItem(key);
      return;
    }
    s.setItem(key, JSON.stringify(entries));
  } catch (err) {
    void err;
    reportError({ kind: "storage.write", reason: "write-failed", area: "muc-call-session-cache" });
  }
}

function removeKey(key: string): void {
  const s = storage();
  if (!s) return;
  try {
    s.removeItem(key);
  } catch (err) {
    void err;
    reportError({ kind: "storage.write", reason: "write-failed", area: "muc-call-session-cache" });
  }
}

export function hydrateMucCallTerminatePendingSessions(
  selfFullJid?: string | null,
  now = new Date(),
): void {
  const selfBare = normalizedBare(selfFullJid?.trim() ?? "");
  if (!selfBare) {
    $mucCallTerminatePendingSessions.set({});
    return;
  }
  const entries = readEntries(selfBare).filter((entry) => !isExpired(entry.updatedAt, now));
  syncPendingEntries(entries.filter((entry) =>
    entry.terminatePending &&
    entry.selfFullJid === selfFullJid?.trim()
  ));
}

export function pendingMucCallTerminateSession(
  sessions: Record<string, CachedMucCallSession>,
  roomJid: string,
  selfFullJid?: string | null,
): CachedMucCallSession | null {
  const room = normalizeMucCallRoomJid(roomJid);
  const self = selfFullJid?.trim() ?? "";
  if (!room || !self) return null;
  return Object.values(sessions).find((entry) =>
    entry.terminatePending &&
    entry.roomJid === room &&
    entry.selfFullJid === self
  ) ?? null;
}

function syncPendingEntries(entries: CachedMucCallSession[]): void {
  const next: Record<string, CachedMucCallSession> = {};
  for (const entry of entries) {
    if (!entry.terminatePending) continue;
    next[sessionKey(entry)] = entry;
  }
  $mucCallTerminatePendingSessions.set(next);
}

function forgetPendingEntriesForRoom(entry: CachedMucCallSession): void {
  const current = $mucCallTerminatePendingSessions.get();
  const next = Object.fromEntries(
    Object.entries(current).filter(([, candidate]) =>
      candidate.roomJid !== entry.roomJid ||
      candidate.selfFullJid !== entry.selfFullJid
    ),
  );
  if (Object.keys(next).length === Object.keys(current).length) return;
  $mucCallTerminatePendingSessions.set(next);
}

function sessionKey(entry: Pick<CachedMucCallSession, "roomJid" | "sid" | "selfFullJid">): string {
  return `${entry.roomJid}\u0000${entry.sid}\u0000${entry.selfFullJid}`;
}

function cacheKey(selfBareJid: string): string {
  return `${CACHE_PREFIX}.${selfBareJid}`;
}

function normalizedBare(jid: string): string {
  return barePeerJid(jid).toLowerCase();
}

function isExpired(timestamp: string, now: Date): boolean {
  const ms = Date.parse(timestamp);
  return !Number.isFinite(ms) || now.getTime() - ms > CACHE_WINDOW_MS;
}

function isCachedMucCallSession(value: unknown): value is CachedMucCallSession {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  return (
    typeof candidate.roomJid === "string" &&
    typeof candidate.sid === "string" &&
    typeof candidate.selfFullJid === "string" &&
    typeof candidate.updatedAt === "string"
  );
  // Optional `media`, `join`, and `terminatePending` are validated
  // (and silently dropped if malformed) inside `normalizeEntry`.
}

function isCallMedia(value: unknown): value is CallMedia {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  return typeof candidate.audio === "boolean" && typeof candidate.video === "boolean";
}

function isLiveKitJoin(value: unknown): value is LiveKitJoin {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  return (
    typeof candidate.url === "string" &&
    typeof candidate.room === "string" &&
    typeof candidate.identity === "string" &&
    typeof candidate.token === "string"
  );
}

function storage(): Storage | null {
  return callCacheSessionStorage(CACHE_PREFIX);
}
