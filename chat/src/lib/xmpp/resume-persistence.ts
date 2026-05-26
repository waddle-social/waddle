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
const JOINED_ROOMS_PREFIX = "waddle.chat.joined-rooms";
const OWNER_LEASE_PREFIX = `${SM_PREFIX}.owner-lease`;
const OWNER_HANDOFF_PREFIX = `${SM_PREFIX}.owner-handoff`;

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
  resource?: string;
};

interface PersistedSmEnvelope extends PersistedSmResumeState {
  savedAt: number;
  ownerId?: string;
  claimId?: string;
}

type ResumeOwner = {
  ownerId: string;
  instanceId: string;
  explicit: boolean;
};

type OwnerLease = {
  ownerId: string;
  instanceId: string;
  updatedAt: number;
};

type OwnerHandoff = {
  ownerId: string;
  instanceId: string;
  expiresAt: number;
};

/**
 * Reject a persisted SM POD older than this window. Servers usually
 * GC the corresponding session much sooner (XEP-0198 §5); 24h is
 * generous but bounds the cost of a guaranteed `<failed/>` on every
 * cold start with a weeks-old POD.
 */
const SM_TTL_MS = 24 * 60 * 60 * 1000;
const OWNER_LEASE_TTL_MS = 45_000;
const OWNER_HEARTBEAT_MS = 15_000;
const OWNER_HANDOFF_TTL_MS = OWNER_LEASE_TTL_MS;
const liveOwnerInstances = new Map<string, string>();
const liveOwnerHeartbeats = new Set<string>();

export interface ResumePersistence {
  loadCatchup(): PersistedReconnectCatchup | null;
  saveCatchup(snapshot: PersistedReconnectCatchup): void;
  clearCatchup(): void;
  loadSm(): PersistedSmResumeState | null;
  consumeSm(): PersistedSmResumeState | null;
  saveSm(state: PersistedSmResumeState): void;
  clearSm(): void;
  preparePagehideHandoff(): void;
  loadJoinedRooms(): string[];
  saveJoinedRooms(roomJids: readonly string[]): void;
  clearJoinedRooms(): void;
}

/** No-op persistence — used in tests / non-browser contexts. */
export const nullResumePersistence: ResumePersistence = {
  loadCatchup: () => null,
  saveCatchup: () => undefined,
  clearCatchup: () => undefined,
  loadSm: () => null,
  consumeSm: () => null,
  saveSm: () => undefined,
  clearSm: () => undefined,
  preparePagehideHandoff: () => undefined,
  loadJoinedRooms: () => [],
  saveJoinedRooms: () => undefined,
  clearJoinedRooms: () => undefined,
};

export function createLocalStorageResumePersistence(accountKey: string, ownerId?: string): ResumePersistence {
  const owner = ownerId ? explicitResumeOwner(ownerId) : resumeOwner();
  const catchupKey = `${CATCHUP_PREFIX}.${accountKey}`;
  const smKey = `${SM_PREFIX}.${accountKey}`;
  const joinedRoomsKey = `${JOINED_ROOMS_PREFIX}.${accountKey}.${owner.ownerId}`;

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
      if (envelope.ownerId !== owner.ownerId) return null;
      if (envelope.claimId) return null;
      const { previd, inboundH, outboundH, resource } = envelope;
      return { previd, inboundH, outboundH, ...(resource ? { resource } : {}) };
    },
    consumeSm() {
      return consumeSmEnvelope(smKey, owner.ownerId);
    },
    saveSm(state) {
      const envelope: PersistedSmEnvelope = { ...state, savedAt: Date.now(), ownerId: owner.ownerId };
      removeKey(`${smKey}.consumed`, "sm-consumed");
      writeJson(smKey, envelope, "sm");
    },
    clearSm() {
      removeKey(smKey, "sm");
      removeKey(`${smKey}.consumed`, "sm-consumed");
    },
    preparePagehideHandoff() {
      markOwnerHandoff(owner);
    },
    loadJoinedRooms() {
      const stored = readJson<string[]>(joinedRoomsKey, isStringArray, "joined-rooms") ?? [];
      return [...new Set(stored.map(normalizeRoomJid).filter(Boolean))];
    },
    saveJoinedRooms(roomJids) {
      writeJson(
        joinedRoomsKey,
        [...new Set(roomJids.map(normalizeRoomJid).filter(Boolean))],
        "joined-rooms",
      );
    },
    clearJoinedRooms() {
      removeKey(joinedRoomsKey, "joined-rooms");
    },
  };
}

function consumeSmEnvelope(key: string, ownerId: string): PersistedSmResumeState | null {
  const s = storage();
  if (!s) return null;
  const consumedKey = `${key}.consumed`;
  try {
    const raw = s.getItem(key);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    if (!isPersistedSmEnvelope(parsed)) return null;
    const consumedMarker = smEnvelopeMarker(raw);
    if (Date.now() - parsed.savedAt > SM_TTL_MS) {
      s.removeItem(key);
      return null;
    }
    if (parsed.ownerId !== ownerId) return null;
    if (parsed.claimId) return null;
    if (s.getItem(consumedKey) === consumedMarker) {
      s.removeItem(key);
      return null;
    }

    const claimId = randomClaimId();
    if (s.getItem(key) !== raw || s.getItem(consumedKey) === consumedMarker) return null;
    s.setItem(key, JSON.stringify({ ...parsed, claimId }));

    const claimedRaw = s.getItem(key);
    if (!claimedRaw) return null;
    const claimed: unknown = JSON.parse(claimedRaw);
    if (!isPersistedSmEnvelope(claimed) || claimed.claimId !== claimId) return null;
    if (s.getItem(consumedKey) === consumedMarker) {
      s.removeItem(key);
      return null;
    }

    s.setItem(consumedKey, consumedMarker);
    s.removeItem(key);
    const { previd, inboundH, outboundH, resource } = claimed;
    return { previd, inboundH, outboundH, ...(resource ? { resource } : {}) };
  } catch (err) {
    reportError("storage.read", err, {
      recoverable: true,
      detail: "resume-persistence consume failed (sm)",
      key,
    });
    return null;
  }
}

function randomClaimId(): string {
  return globalThis.crypto?.randomUUID?.() ?? Math.random().toString(36).slice(2);
}

function explicitResumeOwner(ownerId: string): ResumeOwner {
  return {
    ownerId,
    instanceId: `explicit:${ownerId}`,
    explicit: true,
  };
}

function resumeOwner(): ResumeOwner {
  const s = sessionStorageForOwner();
  if (!s) {
    const ownerId = randomClaimId();
    return { ownerId, instanceId: ownerInstanceId(ownerId), explicit: false };
  }
  const key = `${SM_PREFIX}.owner`;
  try {
    let ownerId = s.getItem(key) || randomClaimId();
    let instanceId = ownerInstanceId(ownerId);
    if (copiedLiveOwnerNeedsRotation(ownerId, instanceId)) {
      ownerId = randomClaimId();
      instanceId = ownerInstanceId(ownerId);
    }
    s.setItem(key, ownerId);
    const owner: ResumeOwner = { ownerId, instanceId, explicit: false };
    claimOwnerLease(owner);
    startOwnerHeartbeat(owner);
    return owner;
  } catch {
    const ownerId = randomClaimId();
    return { ownerId, instanceId: ownerInstanceId(ownerId), explicit: false };
  }
}

function ownerInstanceId(ownerId: string): string {
  const current = liveOwnerInstances.get(ownerId);
  if (current) return current;
  const next = randomClaimId();
  liveOwnerInstances.set(ownerId, next);
  return next;
}

function copiedLiveOwnerNeedsRotation(ownerId: string, instanceId: string): boolean {
  const lease = readOwnerLease(ownerLeaseKey(ownerId));
  if (!lease || lease.instanceId === instanceId || Date.now() - lease.updatedAt > OWNER_LEASE_TTL_MS) {
    return false;
  }
  const handoff = readOwnerHandoff(ownerHandoffKey(ownerId));
  return !handoff || handoff.expiresAt <= Date.now();
}

function claimOwnerLease(owner: ResumeOwner): void {
  if (owner.explicit) return;
  writeJson(ownerLeaseKey(owner.ownerId), {
    ownerId: owner.ownerId,
    instanceId: owner.instanceId,
    updatedAt: Date.now(),
  }, "owner-lease");
  removeKey(ownerHandoffKey(owner.ownerId), "owner-handoff");
}

function startOwnerHeartbeat(owner: ResumeOwner): void {
  if (owner.explicit || liveOwnerHeartbeats.has(owner.ownerId)) return;
  if (typeof document === "undefined") return;
  liveOwnerHeartbeats.add(owner.ownerId);
  const timer = setInterval(() => claimOwnerLease(owner), OWNER_HEARTBEAT_MS);
  (timer as { unref?: () => void }).unref?.();
}

function markOwnerHandoff(owner: ResumeOwner): void {
  writeJson(ownerHandoffKey(owner.ownerId), {
    ownerId: owner.ownerId,
    instanceId: owner.instanceId,
    expiresAt: Date.now() + OWNER_HANDOFF_TTL_MS,
  }, "owner-handoff");
}

function ownerLeaseKey(ownerId: string): string {
  return `${OWNER_LEASE_PREFIX}.${ownerId}`;
}

function ownerHandoffKey(ownerId: string): string {
  return `${OWNER_HANDOFF_PREFIX}.${ownerId}`;
}

function readOwnerLease(key: string): OwnerLease | null {
  return readJson<OwnerLease>(key, isOwnerLease, "owner-lease");
}

function readOwnerHandoff(key: string): OwnerHandoff | null {
  return readJson<OwnerHandoff>(key, isOwnerHandoff, "owner-handoff");
}

function smEnvelopeMarker(raw: string): string {
  let hash = 2166136261;
  for (let i = 0; i < raw.length; i += 1) {
    hash ^= raw.charCodeAt(i);
    hash = Math.imul(hash, 16777619);
  }
  return hash.toString(36);
}

function storage(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function sessionStorageForOwner(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.sessionStorage ?? null;
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

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function isOwnerLease(value: unknown): value is OwnerLease {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  return (
    typeof candidate.ownerId === "string" &&
    typeof candidate.instanceId === "string" &&
    typeof candidate.updatedAt === "number"
  );
}

function isOwnerHandoff(value: unknown): value is OwnerHandoff {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  return (
    typeof candidate.ownerId === "string" &&
    typeof candidate.instanceId === "string" &&
    typeof candidate.expiresAt === "number"
  );
}

function normalizeRoomJid(roomJid: string): string {
  return roomJid.split("/")[0]?.trim().toLowerCase() ?? "";
}

function isPersistedSmEnvelope(value: unknown): value is PersistedSmEnvelope {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  return (
    typeof candidate.previd === "string"
    && typeof candidate.inboundH === "number"
    && typeof candidate.outboundH === "number"
    && (candidate.resource === undefined || typeof candidate.resource === "string")
    && (candidate.ownerId === undefined || typeof candidate.ownerId === "string")
    && (candidate.claimId === undefined || typeof candidate.claimId === "string")
    && typeof candidate.savedAt === "number"
  );
}
