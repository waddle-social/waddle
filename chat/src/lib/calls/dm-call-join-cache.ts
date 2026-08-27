import type { CallMedia, LiveKitJoin } from "./types";
import { barePeerJid } from "@/lib/xmpp/jid";
import { reportError } from "@/lib/telemetry";
import {
  callCacheSessionStorage,
  clearCallCacheKeysWithPrefix,
  localStorageForLegacyPurge,
  purgeLegacyStoragePrefixFromLocalStorage,
} from "./call-token-storage-migration";

const CACHE_PREFIX = "waddle.chat.dm-call-joins";
const MAX_CACHE_ENTRIES = 32;

// Remove bearer tokens written by pre-#1450 builds as soon as this cache is
// initialized. `storage()` repeats the one-shot call so SSR-first imports can
// retry after hydration makes browser storage available.
purgeLegacyStoragePrefixFromLocalStorage(CACHE_PREFIX);
const CACHE_WINDOW_MS = 24 * 60 * 60 * 1000;

type CachedDmCallJoin = {
  peerJid: string;
  sid: string;
  selfFullJid: string;
  remoteFullJid: string;
  media: CallMedia;
  join: LiveKitJoin;
  updatedAt: string;
};

export function rememberDmCallJoin(options: {
  selfBareJid: string;
  entry: CachedDmCallJoin;
  now?: Date;
}): void {
  const selfBare = normalizedBare(options.selfBareJid);
  const entry = normalizeEntry(options.entry);
  if (!selfBare || !entry || normalizedBare(entry.selfFullJid) !== selfBare) return;

  const now = options.now ?? new Date();
  const entries = readEntries(selfBare)
    .filter((candidate) => !isExpired(candidate.updatedAt, now))
    .filter((candidate) =>
      candidate.sid !== entry.sid ||
      candidate.peerJid !== entry.peerJid ||
      candidate.selfFullJid !== entry.selfFullJid
    );
  entries.unshift(entry);
  writeEntries(selfBare, entries.slice(0, MAX_CACHE_ENTRIES));
}

export function readDmCallJoin(options: {
  selfBareJid: string;
  peerJid: string;
  sid: string;
  selfFullJid?: string | null;
  now?: Date;
}): CachedDmCallJoin | null {
  const selfBare = normalizedBare(options.selfBareJid);
  const peerJid = normalizedBare(options.peerJid);
  const selfFullJid = options.selfFullJid?.trim() ?? "";
  if (!selfBare || !peerJid || !options.sid || !selfFullJid) return null;

  const now = options.now ?? new Date();
  let changed = false;
  const freshEntries = readEntries(selfBare).filter((entry) => {
    const keep = !isExpired(entry.updatedAt, now);
    changed ||= !keep;
    return keep;
  });
  if (changed) writeEntries(selfBare, freshEntries);

  return freshEntries.find((entry) =>
    entry.peerJid === peerJid &&
    entry.sid === options.sid &&
    entry.selfFullJid === selfFullJid
  ) ?? null;
}

export function forgetDmCallJoin(options: {
  selfBareJid: string;
  peerJid: string;
  sid: string;
}): void {
  const selfBare = normalizedBare(options.selfBareJid);
  const peerJid = normalizedBare(options.peerJid);
  if (!selfBare || !peerJid || !options.sid) return;
  const entries = readEntries(selfBare);
  const next = entries.filter((entry) => entry.peerJid !== peerJid || entry.sid !== options.sid);
  if (next.length !== entries.length) writeEntries(selfBare, next);
}

export function clearDmCallJoinCacheForAccount(selfBareJid: string): void {
  const selfBare = normalizedBare(selfBareJid);
  if (!selfBare) return;
  removeKey(cacheKey(selfBare));
}

export function clearAllDmCallJoinCacheForTests(): void {
  clearCallCacheKeysWithPrefix(storage(), CACHE_PREFIX, "dm call join cache clear failed");
  clearCallCacheKeysWithPrefix(
    localStorageForLegacyPurge(),
    CACHE_PREFIX,
    "dm call join cache clear failed",
  );
}

function normalizeEntry(entry: CachedDmCallJoin): CachedDmCallJoin | null {
  const peerJid = normalizedBare(entry.peerJid);
  const remoteFullJid = entry.remoteFullJid.trim();
  const selfFullJid = entry.selfFullJid.trim();
  if (!peerJid || !entry.sid || !remoteFullJid || !selfFullJid) return null;
  if (normalizedBare(remoteFullJid) !== peerJid) return null;
  if (!isCallMedia(entry.media) || !isLiveKitJoin(entry.join)) return null;
  if (entry.join.identity !== selfFullJid) return null;
  if (Number.isNaN(Date.parse(entry.updatedAt))) return null;
  return {
    peerJid,
    sid: entry.sid,
    selfFullJid,
    remoteFullJid,
    media: entry.media,
    join: entry.join,
    updatedAt: entry.updatedAt,
  };
}

function readEntries(selfBareJid: string): CachedDmCallJoin[] {
  const s = storage();
  if (!s) return [];
  const key = cacheKey(selfBareJid);
  try {
    const raw = s.getItem(key);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed
      .map((entry) => isCachedDmCallJoin(entry) ? normalizeEntry(entry) : null)
      .filter((entry): entry is CachedDmCallJoin => !!entry);
  } catch (err) {
    void err;
    reportError({ kind: "storage.read", reason: "read-failed", area: "dm-call-join-cache" });
    return [];
  }
}

function writeEntries(selfBareJid: string, entries: CachedDmCallJoin[]): void {
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
    reportError({ kind: "storage.write", reason: "write-failed", area: "dm-call-join-cache" });
  }
}

function removeKey(key: string): void {
  const s = storage();
  if (!s) return;
  try {
    s.removeItem(key);
  } catch (err) {
    void err;
    reportError({ kind: "storage.write", reason: "write-failed", area: "dm-call-join-cache" });
  }
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

function isCachedDmCallJoin(value: unknown): value is CachedDmCallJoin {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  return (
    typeof candidate.peerJid === "string" &&
    typeof candidate.sid === "string" &&
    typeof candidate.selfFullJid === "string" &&
    typeof candidate.remoteFullJid === "string" &&
    isCallMedia(candidate.media) &&
    isLiveKitJoin(candidate.join) &&
    typeof candidate.updatedAt === "string"
  );
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
