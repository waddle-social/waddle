import { barePeerJid } from "@/lib/xmpp/jid";
import { normalizeMucCallRoomJid } from "./muc-call-presence";
import { reportError } from "@/lib/telemetry";
import { map } from "nanostores";

const CACHE_PREFIX = "waddle.chat.muc-call-sessions";
const CACHE_WINDOW_MS = 24 * 60 * 60 * 1000;
const MAX_CACHE_ENTRIES = 64;

type CachedMucCallSession = {
  roomJid: string;
  sid: string;
  selfFullJid: string;
  updatedAt: string;
  terminatePending?: boolean;
};

export const $mucCallTerminatePendingSessions = map<Record<string, CachedMucCallSession>>({});

export function rememberMucCallSession(options: {
  roomJid: string;
  sid: string;
  selfFullJid?: string | null;
  now?: Date;
}): void {
  const entry = normalizeEntry({
    roomJid: options.roomJid,
    sid: options.sid,
    selfFullJid: options.selfFullJid?.trim() ?? "",
    updatedAt: options.now?.toISOString() ?? new Date().toISOString(),
  });
  if (!entry) return;
  forgetPendingEntry(entry);
  const selfBare = normalizedBare(entry.selfFullJid);
  if (!selfBare) return;
  const now = options.now ?? new Date();
  const entries = readEntries(selfBare)
    .filter((candidate) => !isExpired(candidate.updatedAt, now))
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
  now?: Date;
}): void {
  const entry = normalizeEntry({
    roomJid: options.roomJid,
    sid: options.sid,
    selfFullJid: options.selfFullJid?.trim() ?? "",
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
        ? { ...candidate, terminatePending: true, updatedAt: entry.updatedAt }
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

export function clearAllMucCallSessionCacheForTests(): void {
  $mucCallTerminatePendingSessions.set({});
  const s = storage();
  if (!s) return;
  try {
    const keys: string[] = [];
    for (let index = 0; index < s.length; index += 1) {
      const key = s.key(index);
      if (key?.startsWith(`${CACHE_PREFIX}.`)) keys.push(key);
    }
    for (const key of keys) s.removeItem(key);
  } catch (err) {
    reportError("storage.write", err, {
      recoverable: true,
      detail: "muc call session cache clear failed",
    });
  }
}

function normalizeEntry(entry: CachedMucCallSession): CachedMucCallSession | null {
  const roomJid = normalizeMucCallRoomJid(entry.roomJid);
  const selfFullJid = entry.selfFullJid.trim();
  const sid = entry.sid.trim();
  if (!roomJid || !selfFullJid || !sid) return null;
  if (!normalizedBare(selfFullJid)) return null;
  if (Number.isNaN(Date.parse(entry.updatedAt))) return null;
  return {
    roomJid,
    sid,
    selfFullJid,
    updatedAt: entry.updatedAt,
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
    reportError("storage.read", err, {
      recoverable: true,
      detail: "muc call session cache read failed",
      key,
    });
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
    reportError("storage.write", err, {
      recoverable: true,
      detail: "muc call session cache write failed",
      key,
    });
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

export function hasPendingMucCallTerminateSession(
  sessions: Record<string, CachedMucCallSession>,
  roomJid: string,
  selfFullJid?: string | null,
): boolean {
  const room = normalizeMucCallRoomJid(roomJid);
  const self = selfFullJid?.trim() ?? "";
  if (!room || !self) return false;
  return Object.values(sessions).some((entry) =>
    entry.terminatePending &&
    entry.roomJid === room &&
    entry.selfFullJid === self
  );
}

function syncPendingEntries(entries: CachedMucCallSession[]): void {
  const next: Record<string, CachedMucCallSession> = {};
  for (const entry of entries) {
    if (!entry.terminatePending) continue;
    next[sessionKey(entry)] = entry;
  }
  $mucCallTerminatePendingSessions.set(next);
}

function forgetPendingEntry(entry: CachedMucCallSession): void {
  const current = $mucCallTerminatePendingSessions.get();
  const key = sessionKey(entry);
  if (!(key in current)) return;
  const next = { ...current };
  delete next[key];
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
}

function storage(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}
