/**
 * Persisted resume state across page reloads.
 *
 * Two kinds of state survive a full reload:
 *
 *   * MAM catch-up cursors (`PersistedReconnectCatchup`), keyed per account — the
 *     timestamp + optional XEP-0313 archive UID + dedupe seen-ids
 *     for every DM peer and MUC room the user has seen. Without
 *     this, a hard reload (cold start, mobile Safari eviction)
 *     loses every cursor and the next `session:started` returns
 *     `[]` from `ReconnectCatchup.onSessionStarted()` — so MAM
 *     catch-up does NOT run and missed messages are only
 *     re-fetched if the user manually scrolls back.
 *
 *   * XEP-0198 Stream Management resume state
 *     (`PersistedSmResumeState`), keyed per account and browser owner — the `previd` plus inbound /
 *     outbound stanza counts and advertised resume window. The richer
 *     "handle" form from the WASM client is a live JS object that cannot
 *     be serialized; the POD `{previd, inboundH, outboundH}` triple can be.
 *     When the native XEP-0198 queue still has unhandled outbound stanzas,
 *     their XML is serialized too so `doConnect` can restore sender
 *     responsibility after a full reload instead of creating a false
 *     resume state with an empty replay queue.
 *
 * The shape mirrors `outbound-queue-store.ts` (same `waddle.chat.*`
 * prefix family, account-scoped catch-up and account-and-owner-scoped SM
 * key namespacing, and the same defensive
 * read/write error handling around localStorage availability and
 * quota) so the storage surface stays uniform.
 */

import { reportError } from "@/lib/telemetry";
import { bareJidKey } from "./jid";
import {
  ROOM_CATALOG_FINGERPRINT_FIELDS,
  type RoomAutoJoinBlock,
} from "./room-auto-join-policy";
import type { RoomCatalogFingerprintField } from "./types";

const CATCHUP_PREFIX = "waddle.chat.resume-cursors";
const SM_PREFIX = "waddle.chat.sm-resume";
const JOINED_ROOMS_PREFIX = "waddle.chat.joined-rooms";
const AUTO_JOIN_BLOCKS_PREFIX = "waddle.chat.auto-join-blocks";
const OWNER_LEASE_PREFIX = `${SM_PREFIX}.owner-lease`;
const OWNER_HANDOFF_PREFIX = `${SM_PREFIX}.owner-handoff`;

type PersistedSeenCursor = {
  timestamp: string;
  scope?: "account" | "muc-occupant";
  archiveId?: string;
  archiveTimestamp?: string;
  archiveSeenIds?: string[];
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
  resumable?: boolean;
  inboundH: number;
  outboundH: number;
  maxResumeSeconds?: number;
  unhandledOutboundEntries?: Array<{ xml: string; sentAt: string }>;
  resource?: string;
};

type PersistedAutoJoinBlock = RoomAutoJoinBlock;

interface PersistedSmEnvelope extends PersistedSmResumeState {
  savedAt: number;
  ownerId?: string;
  ownerInstanceId?: string;
  claimId?: string;
}

type PersistedSmConsumedMarker = {
  marker: string;
  savedAt: number;
  ownerId?: string;
  ownerInstanceId?: string;
};

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
 * Fallback resumption window for older persisted PODs that predate
 * `maxResumeSeconds`. This mirrors Waddle server's default XEP-0198
 * max, so old snapshots fail closed instead of lingering for hours.
 */
const DEFAULT_SM_MAX_RESUME_SECONDS = 300;
const SM_SAVED_AT_FUTURE_SKEW_MS = 60_000;
const OWNER_LEASE_TTL_MS = 45_000;
const OWNER_HEARTBEAT_MS = 15_000;
const OWNER_HANDOFF_TTL_MS = OWNER_LEASE_TTL_MS;
const MAX_SM_OWNER_SLOTS = 64;
const liveOwnerInstances = new Map<string, string>();

type ResumeOwnerTimerDriver = {
  setInterval: (callback: () => void, delayMs: number) => ReturnType<typeof setInterval>;
  clearInterval: (timer: ReturnType<typeof setInterval>) => void;
};

export type ResumePersistenceOptions = {
  /** Test seam: production uses the browser timer functions. */
  ownerTimerDriver?: ResumeOwnerTimerDriver;
};

type OwnerHeartbeatRegistration = {
  ownerId: string;
  instanceId: string;
  timer: ReturnType<typeof setInterval>;
  refCount: number;
  timerDriver: ResumeOwnerTimerDriver;
};

const liveOwnerHeartbeats = new Map<string, OwnerHeartbeatRegistration>();

/**
 * `setInterval` is normally non-reentrant, but a test or host scheduler can
 * synchronously construct another persistence instance before returning (or
 * throwing). Keep the provisional installation identity separate from the
 * live registration so a failed older attempt cannot roll back its successor.
 */
type OwnerHeartbeatAttempt = {
  /** The attempt that was scheduling when this nested attempt began. */
  parent: OwnerHeartbeatAttempt | null;
  supersededBy: OwnerHeartbeatAttempt | null;
  /**
   * A timer that reached the live-registration CAS.  Merely entering a
   * nested `setInterval` is not enough: that scheduler may throw before it
   * installs anything, in which case its still-running parent must retain
   * the lease it already claimed.
   */
  installed: OwnerHeartbeatRegistration | null;
  phase: "scheduling" | "installed" | "failed" | "superseded";
};

const pendingOwnerHeartbeatAttempts = new Map<string, OwnerHeartbeatAttempt>();
const browserTimerDriver: ResumeOwnerTimerDriver = {
  setInterval: (callback, delayMs) => setInterval(callback, delayMs),
  clearInterval: (timer) => clearInterval(timer),
};

export interface ResumePersistence {
  /** Release this client owner's lease heartbeat. Idempotent. */
  dispose(): void;
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
  loadAutoJoinBlocks?(): PersistedAutoJoinBlock[];
  saveAutoJoinBlocks?(blocks: readonly PersistedAutoJoinBlock[]): void;
  clearAutoJoinBlocks?(): void;
}

/** No-op persistence — used in tests / non-browser contexts. */
export const nullResumePersistence: ResumePersistence = {
  dispose: () => undefined,
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
  loadAutoJoinBlocks: () => [],
  saveAutoJoinBlocks: () => undefined,
  clearAutoJoinBlocks: () => undefined,
};

export function createLocalStorageResumePersistence(
  accountKey: string,
  ownerId?: string,
  options: ResumePersistenceOptions = {},
): ResumePersistence {
  const owner = ownerId ? explicitResumeOwner(ownerId) : resumeOwner(accountKey);
  const releaseOwnerHeartbeat = retainOwnerHeartbeat(
    accountKey,
    owner,
    options.ownerTimerDriver ?? browserTimerDriver,
  );
  let disposed = false;
  // Length-prefix the account segment so prefix enumeration cannot make
  // `alice@example.com` consume `alice@example.com.evil` shards.
  const catchupKeyPrefix = `${CATCHUP_PREFIX}.${accountKey.length}:${accountKey}`;
  const catchupKey = `${catchupKeyPrefix}.${owner.ownerId}`;
  // A stream-management tail belongs to its tab owner, not merely the
  // account. Length prefixes keep account and owner enumeration disjoint.
  const smAccountKeyPrefix = `${SM_PREFIX}.${accountKey.length}:${accountKey}`;
  const smKey = `${smAccountKeyPrefix}.${owner.ownerId.length}:${owner.ownerId}`;
  const joinedRoomsKey = `${JOINED_ROOMS_PREFIX}.${accountKey}.${owner.ownerId}`;
  const autoJoinBlocksKey = `${AUTO_JOIN_BLOCKS_PREFIX}.${accountKey}.${owner.ownerId}`;

  return {
    dispose() {
      if (disposed) return;
      disposed = true;
      releaseOwnerHeartbeat();
    },
    loadCatchup() {
      if (disposed) return null;
      return readCatchupShards(catchupKeyPrefix);
    },
    saveCatchup(snapshot) {
      if (disposed) return;
      writeJson(catchupKey, snapshot, "catchup");
    },
    clearCatchup() {
      if (disposed) return;
      removeKeysWithPrefix(`${catchupKeyPrefix}.`, "catchup");
    },
    loadSm() {
      if (disposed) return null;
      gcSmOwnerSlots(smAccountKeyPrefix);
      // A consume tombstone means responsibility may already have moved. Do
      // not even expose its resource to a fresh connection before the later
      // consume path can reject it.
      if (storage()?.getItem(`${smKey}.consumed`)) return null;
      const envelope = readJson<PersistedSmEnvelope>(smKey, isPersistedSmEnvelope, "sm");
      if (!envelope) return null;
      // Drop entries past the advertised resume window — the server has GC'd the
      // corresponding session, so feeding the POD back to
      // `with_resume_state` is a guaranteed `<failed/>`. Returning
      // null lets `doConnect` fall through to a fresh bind.
      if (smEnvelopeExpired(envelope)) {
        removeKey(smKey, "sm");
        return null;
      }
      if (envelope.ownerId !== owner.ownerId) return null;
      if (envelope.claimId) return null;
      const { previd, inboundH, outboundH, maxResumeSeconds, resource, unhandledOutboundEntries } = envelope;
      return {
        previd,
        inboundH,
        outboundH,
        ...(maxResumeSeconds ? { maxResumeSeconds } : {}),
        ...(unhandledOutboundEntries?.length ? { unhandledOutboundEntries: unhandledOutboundEntries.map((entry) => ({ ...entry })) } : {}),
        ...(resource ? { resource } : {}),
      };
    },
    consumeSm() {
      if (disposed) return null;
      gcSmOwnerSlots(smAccountKeyPrefix);
      return consumeSmEnvelope(smKey, owner);
    },
    saveSm(state) {
      if (disposed) return;
      gcSmOwnerSlots(smAccountKeyPrefix);
      if (!canPersistSmOwnerSlot(smAccountKeyPrefix, smKey)) {
        reportError("storage.quota", new Error("SM owner-slot retention is full"), {
          recoverable: true,
          detail: "resume-persistence retained every live owner tail",
          storage_area: "sm-resume",
        });
        return;
      }
      const envelope: PersistedSmEnvelope = {
        ...state,
        savedAt: Date.now(),
        ownerId: owner.ownerId,
        ownerInstanceId: owner.instanceId,
      };
      removeKey(`${smKey}.consumed`, "sm-consumed");
      writeJson(smKey, envelope, "sm");
    },
    clearSm() {
      if (disposed) return;
      removeSmEnvelopeIfOwned(smKey, owner);
      removeSmConsumedMarkerIfOwned(`${smKey}.consumed`, owner);
    },
    preparePagehideHandoff() {
      if (disposed) return;
      markOwnerHandoff(accountKey, owner);
    },
    loadJoinedRooms() {
      if (disposed) return [];
      const stored = readJson<string[]>(joinedRoomsKey, isStringArray, "joined-rooms") ?? [];
      return [...new Set(stored.map(normalizeRoomJid).filter(Boolean))];
    },
    saveJoinedRooms(roomJids) {
      if (disposed) return;
      writeJson(
        joinedRoomsKey,
        [...new Set(roomJids.map(normalizeRoomJid).filter(Boolean))],
        "joined-rooms",
      );
    },
    clearJoinedRooms() {
      if (disposed) return;
      removeKey(joinedRoomsKey, "joined-rooms");
    },
    loadAutoJoinBlocks() {
      if (disposed) return [];
      const stored = readJson<PersistedAutoJoinBlock[]>(
        autoJoinBlocksKey,
        isPersistedAutoJoinBlockArray,
        "auto-join-blocks",
      ) ?? [];
      const normalized = new Map<string, PersistedAutoJoinBlock>();
      for (const block of stored) {
        const roomJid = normalizeRoomJid(block.roomJid);
        if (!roomJid) continue;
        normalized.set(roomJid, {
          roomJid,
          condition: block.condition,
          ...(block.catalogFingerprint !== undefined
            ? { catalogFingerprint: block.catalogFingerprint }
            : {}),
          ...(block.catalogFingerprintFields
            ? {
              catalogFingerprintFields:
                [...block.catalogFingerprintFields],
            }
            : {}),
        });
      }
      return [...normalized.values()];
    },
    saveAutoJoinBlocks(blocks) {
      if (disposed) return;
      writeJson(
        autoJoinBlocksKey,
        blocks.map((block) => ({
          roomJid: normalizeRoomJid(block.roomJid),
          condition: block.condition,
          ...(block.catalogFingerprint !== undefined
            ? { catalogFingerprint: block.catalogFingerprint }
            : {}),
          ...(block.catalogFingerprintFields
            ? {
              catalogFingerprintFields:
                [...block.catalogFingerprintFields],
            }
            : {}),
        })).filter((block) => !!block.roomJid),
        "auto-join-blocks",
      );
    },
    clearAutoJoinBlocks() {
      if (disposed) return;
      removeKey(autoJoinBlocksKey, "auto-join-blocks");
    },
  };
}

function consumeSmEnvelope(key: string, owner: ResumeOwner): PersistedSmResumeState | null {
  const s = storage();
  if (!s) return null;
  const consumedKey = `${key}.consumed`;
  try {
    const raw = s.getItem(key);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    if (!isPersistedSmEnvelope(parsed)) return null;
    const consumedMarker = smEnvelopeMarker(raw);
    if (smEnvelopeExpired(parsed)) {
      s.removeItem(key);
      return null;
    }
    if (parsed.ownerId !== owner.ownerId) return null;
    if (parsed.claimId) return null;
    if (smConsumedMarkerMatches(s.getItem(consumedKey), consumedMarker)) {
      s.removeItem(key);
      return null;
    }

    const claimId = randomClaimId();
    if (s.getItem(key) !== raw || smConsumedMarkerMatches(s.getItem(consumedKey), consumedMarker)) return null;
    s.setItem(key, JSON.stringify({ ...parsed, claimId }));

    const claimedRaw = s.getItem(key);
    if (!claimedRaw) return null;
    const claimed: unknown = JSON.parse(claimedRaw);
    if (!isPersistedSmEnvelope(claimed) || claimed.claimId !== claimId) return null;
    if (smConsumedMarkerMatches(s.getItem(consumedKey), consumedMarker)) {
      s.removeItem(key);
      return null;
    }

    s.setItem(consumedKey, JSON.stringify({
      marker: consumedMarker,
      savedAt: Date.now(),
      ownerId: owner.ownerId,
      ownerInstanceId: owner.instanceId,
    }));
    s.removeItem(key);
    const { previd, inboundH, outboundH, maxResumeSeconds, resource, unhandledOutboundEntries } = claimed;
    return {
      previd,
      inboundH,
      outboundH,
      ...(maxResumeSeconds ? { maxResumeSeconds } : {}),
      ...(unhandledOutboundEntries?.length ? { unhandledOutboundEntries: unhandledOutboundEntries.map((entry) => ({ ...entry })) } : {}),
      ...(resource ? { resource } : {}),
    };
  } catch (err) {
    reportError("storage.read", err, {
      recoverable: true,
      detail: "resume-persistence consume failed (sm)",
      storage_area: "sm-resume",
    });
    return null;
  }
}

function smConsumedMarkerMatches(raw: string | null, marker: string): boolean {
  if (!raw) return false;
  try {
    const parsed: unknown = JSON.parse(raw);
    return !isPersistedSmConsumedMarker(parsed) || parsed.marker === marker;
  } catch {
    // An uncertain marker must never permit a second resume attempt.
    return true;
  }
}

function gcSmOwnerSlots(accountKeyPrefix: string): void {
  const prefix = `${accountKeyPrefix}.`;
  for (const key of storageKeysWithPrefix(prefix)) {
    const slot = smSlotKeyInfo(accountKeyPrefix, key);
    if (!slot) {
      removeKey(key, "sm");
      continue;
    }
    if (slot.consumed) {
      const marker = readJson<PersistedSmConsumedMarker>(key, isPersistedSmConsumedMarker, "sm-consumed");
      const snapshotKey = key.slice(0, -".consumed".length);
      if (!marker || smMarkerExpired(marker) || storage()?.getItem(snapshotKey)) {
        // A marker and a sibling snapshot can coexist only across an
        // interrupted claim. Preserve at-most-once responsibility by
        // discarding both rather than letting damaged storage replay it.
        removeKey(key, "sm-consumed");
        removeKey(snapshotKey, "sm");
      }
      continue;
    }
    const envelope = readJson<PersistedSmEnvelope>(key, isPersistedSmEnvelope, "sm");
    if (
      !envelope
      || smEnvelopeExpired(envelope)
      || envelope.ownerId !== slot.ownerId
      || !!envelope.claimId
    ) {
      removeKey(key, "sm");
      removeKey(`${key}.consumed`, "sm-consumed");
    }
  }
}

function smSlotKeyInfo(
  accountKeyPrefix: string,
  key: string,
): { ownerId: string; consumed: boolean } | null {
  const prefix = `${accountKeyPrefix}.`;
  if (!key.startsWith(prefix)) return null;
  const encodedOwner = key.slice(prefix.length);
  const separator = encodedOwner.indexOf(":");
  if (separator <= 0) return null;
  const declaredLengthText = encodedOwner.slice(0, separator);
  if (!/^[1-9]\d*$/.test(declaredLengthText)) return null;
  const declaredLength = Number(declaredLengthText);
  const ownerAndMarker = encodedOwner.slice(separator + 1);
  if (!Number.isSafeInteger(declaredLength)) return null;
  const ownerId = ownerAndMarker.slice(0, declaredLength);
  if (ownerId.length !== declaredLength) return null;
  if (ownerAndMarker.length === declaredLength) return { ownerId, consumed: false };
  return ownerAndMarker === `${ownerId}.consumed` ? { ownerId, consumed: true } : null;
}

function canPersistSmOwnerSlot(accountKeyPrefix: string, ownKey: string): boolean {
  const s = storage();
  if (!s) return false;
  if (s.getItem(ownKey)) return true;
  const ownSlot = smSlotKeyInfo(accountKeyPrefix, ownKey);
  if (!ownSlot) return false;
  const retainedOwners = new Set(
    storageKeysWithPrefix(`${accountKeyPrefix}.`)
      .flatMap((key) => {
        const slot = smSlotKeyInfo(accountKeyPrefix, key);
        return slot ? [slot.ownerId] : [];
      }),
  );
  if (retainedOwners.has(ownSlot.ownerId)) return true;
  // Never prune a still-valid tail just to make room for another tab. The
  // bounded owner window fails closed for the new tab instead.
  return retainedOwners.size < MAX_SM_OWNER_SLOTS;
}

function smMarkerExpired(marker: PersistedSmConsumedMarker): boolean {
  const ageMs = Date.now() - marker.savedAt;
  return ageMs < -SM_SAVED_AT_FUTURE_SKEW_MS || ageMs > DEFAULT_SM_MAX_RESUME_SECONDS * 1000;
}

function randomClaimId(): string {
  return globalThis.crypto?.randomUUID?.() ?? Math.random().toString(36).slice(2);
}

function smEnvelopeExpired(envelope: PersistedSmEnvelope): boolean {
  const maxResumeSeconds = envelope.maxResumeSeconds ?? DEFAULT_SM_MAX_RESUME_SECONDS;
  const ageMs = Date.now() - envelope.savedAt;
  if (ageMs < -SM_SAVED_AT_FUTURE_SKEW_MS) return true;
  return ageMs > maxResumeSeconds * 1000;
}

function explicitResumeOwner(ownerId: string): ResumeOwner {
  return {
    ownerId,
    instanceId: `explicit:${ownerId}`,
    explicit: true,
  };
}

function resumeOwner(accountKey: string): ResumeOwner {
  const s = sessionStorageForOwner();
  if (!s) {
    const ownerId = randomClaimId();
    return { ownerId, instanceId: ownerInstanceId(accountKey, ownerId), explicit: false };
  }
  const key = `${SM_PREFIX}.owner`;
  try {
    const inheritedOwnerId = s.getItem(key);
    let ownerId = inheritedOwnerId || randomClaimId();
    let instanceId = ownerInstanceId(accountKey, ownerId);
    if (inheritedOwnerId && copiedLiveOwnerNeedsRotation(accountKey, ownerId, instanceId)) {
      // `ownerInstanceId` reserves the inherited identity before the complete
      // reload handoff is checked. If that handoff fails, release only this
      // provisional reservation before selecting the rotated owner so a later
      // terminal release cannot leave an unreachable registry entry behind.
      releaseOwnerInstance(accountKey, { ownerId, instanceId, explicit: false });
      ownerId = randomClaimId();
      instanceId = ownerInstanceId(accountKey, ownerId);
    }
    s.setItem(key, ownerId);
    return { ownerId, instanceId, explicit: false };
  } catch {
    const ownerId = randomClaimId();
    return { ownerId, instanceId: ownerInstanceId(accountKey, ownerId), explicit: false };
  }
}

function ownerInstanceId(accountKey: string, ownerId: string): string {
  const key = ownerRegistryKey(accountKey, ownerId);
  const current = liveOwnerInstances.get(key);
  if (current) return current;
  const next = randomClaimId();
  liveOwnerInstances.set(key, next);
  return next;
}

function copiedLiveOwnerNeedsRotation(accountKey: string, ownerId: string, instanceId: string): boolean {
  const lease = readOwnerLease(ownerLeaseKey(accountKey, ownerId));
  // A second factory in this live JS document shares the current owner; it is
  // not a copied sessionStorage identity. Every new document must instead
  // prove the complete reload handoff below before retaining the old owner.
  if (lease?.instanceId === instanceId) {
    return false;
  }
  const handoff = readOwnerHandoff(ownerHandoffKey(accountKey, ownerId));
  return !isConfirmedSameTabReloadHandoff(ownerId, lease, handoff);
}

/**
 * sessionStorage is copied when a tab is duplicated, so a fresh document can
 * inherit an owner's identifier. A pagehide handoff is only safe to honour
 * for a confirmed reload of the same document: the handoff must still be
 * live and must have been produced by the lease holder that is being
 * replaced. Navigation, history restores, prerender activation, and missing
 * timing data all rotate so they cannot consume another tab's SM tail.
 */
function isConfirmedSameTabReloadHandoff(
  ownerId: string,
  lease: OwnerLease | null,
  handoff: OwnerHandoff | null,
): boolean {
  const now = Date.now();
  return !!lease
    && lease.ownerId === ownerId
    && lease.updatedAt <= now
    && now - lease.updatedAt <= OWNER_LEASE_TTL_MS
    && handoff?.ownerId === ownerId
    && handoff.instanceId === lease.instanceId
    && handoff.expiresAt > now
    && navigationWasReload();
}

function navigationWasReload(): boolean {
  if (typeof performance === "undefined") return false;
  const navigation = performance.getEntriesByType("navigation")[0] as PerformanceNavigationTiming | undefined;
  return navigation?.type === "reload";
}

function claimOwnerLease(accountKey: string, owner: ResumeOwner): void {
  if (owner.explicit) return;
  writeJson(ownerLeaseKey(accountKey, owner.ownerId), {
    ownerId: owner.ownerId,
    instanceId: owner.instanceId,
    updatedAt: Date.now(),
  }, "owner-lease");
  removeKey(ownerHandoffKey(accountKey, owner.ownerId), "owner-handoff");
}

function retainOwnerHeartbeat(
  accountKey: string,
  owner: ResumeOwner,
  timerDriver: ResumeOwnerTimerDriver,
): () => void {
  if (owner.explicit) {
    return () => undefined;
  }
  if (typeof document === "undefined" && timerDriver === browserTimerDriver) {
    return () => releaseOwnerInstance(accountKey, owner);
  }
  const key = ownerRegistrationKey(accountKey, owner);
  const existing = liveOwnerHeartbeats.get(key);
  if (existing) {
    existing.refCount += 1;
    return () => releaseOwnerHeartbeat(accountKey, owner, existing);
  }
  const priorAttempt = pendingOwnerHeartbeatAttempts.get(key) ?? null;
  const attempt: OwnerHeartbeatAttempt = {
    parent: priorAttempt,
    supersededBy: null,
    installed: null,
    phase: "scheduling",
  };
  if (priorAttempt) priorAttempt.supersededBy = attempt;
  pendingOwnerHeartbeatAttempts.set(key, attempt);
  claimOwnerLease(accountKey, owner);
  let registration: OwnerHeartbeatRegistration;
  try {
    registration = {
      ownerId: owner.ownerId,
      instanceId: owner.instanceId,
      timer: timerDriver.setInterval(() => {
        if (liveOwnerHeartbeats.get(key) !== registration) return;
        claimOwnerLease(accountKey, owner);
      }, OWNER_HEARTBEAT_MS),
      refCount: 1,
      timerDriver,
    };
  } catch (error) {
    attempt.phase = "failed";
    if (pendingOwnerHeartbeatAttempts.get(key) === attempt) {
      // Restore an outer scheduling attempt.  It has already claimed the
      // owner lease and may still receive a timer from its driver after this
      // nested scheduler fails.
      if (attempt.parent?.phase === "scheduling") {
        pendingOwnerHeartbeatAttempts.set(key, attempt.parent);
      } else {
        pendingOwnerHeartbeatAttempts.delete(key);
      }
    }
    // A scheduler failure may have re-entered and installed a successor for
    // this same owner before throwing. It may also be a nested attempt whose
    // parent is still scheduling.  Roll back only when neither can retain the
    // claimed identity; otherwise a failed nested scheduler would tear down
    // the outer attempt's lease, session owner, and eventual ref-count.
    if (!liveInstalledSuccessor(key, attempt) && attempt.parent?.phase !== "scheduling") {
      removeOwnerLeaseIfOwned(accountKey, owner);
      releaseOwnerInstance(accountKey, owner);
      removeStoredOwnerIdIfOwned(owner);
    }
    throw error;
  }
  if (liveInstalledSuccessor(key, attempt)) {
    // A nested attempt reached the exact live-registration CAS. This older
    // caller retained no reference, so discard only its unregistered timer;
    // its failure or disposal cannot roll back the successor.
    timerDriver.clearInterval(registration.timer);
    attempt.phase = "superseded";
    if (pendingOwnerHeartbeatAttempts.get(key) === attempt) {
      pendingOwnerHeartbeatAttempts.delete(key);
    }
    return () => undefined;
  }
  (registration.timer as { unref?: () => void }).unref?.();
  liveOwnerHeartbeats.set(key, registration);
  attempt.installed = registration;
  attempt.phase = "installed";
  if (pendingOwnerHeartbeatAttempts.get(key) === attempt) {
    pendingOwnerHeartbeatAttempts.delete(key);
  }
  return () => releaseOwnerHeartbeat(accountKey, owner, registration);
}

/**
 * Returns true only for a nested attempt that completed the exact
 * live-registration installation and is still live.  A nested scheduler that
 * merely started (then threw) cannot suppress its outer attempt.
 */
function liveInstalledSuccessor(key: string, attempt: OwnerHeartbeatAttempt): boolean {
  let successor = attempt.supersededBy;
  while (successor) {
    if (successor.installed && liveOwnerHeartbeats.get(key) === successor.installed) {
      return true;
    }
    successor = successor.supersededBy;
  }
  return false;
}

function releaseOwnerHeartbeat(
  accountKey: string,
  owner: ResumeOwner,
  registration: OwnerHeartbeatRegistration,
): void {
  const key = ownerRegistrationKey(accountKey, owner);
  if (liveOwnerHeartbeats.get(key) !== registration) return;
  registration.refCount -= 1;
  if (registration.refCount > 0) return;
  registration.timerDriver.clearInterval(registration.timer);
  liveOwnerHeartbeats.delete(key);
  releaseOwnerInstance(accountKey, owner);
  removeOwnerLeaseIfOwned(accountKey, owner);
  removeOwnerHandoffIfOwned(accountKey, owner);
}

function releaseOwnerInstance(accountKey: string, owner: ResumeOwner): void {
  const ownerKey = ownerRegistryKey(accountKey, owner.ownerId);
  if (liveOwnerInstances.get(ownerKey) === owner.instanceId) {
    liveOwnerInstances.delete(ownerKey);
  }
}

function removeStoredOwnerIdIfOwned(owner: ResumeOwner): void {
  if (owner.explicit) return;
  const storage = sessionStorageForOwner();
  if (!storage) return;
  const key = `${SM_PREFIX}.owner`;
  try {
    if (storage.getItem(key) === owner.ownerId) storage.removeItem(key);
  } catch {
    // sessionStorage failure cannot make a failed scheduler retain a lease.
  }
}

function markOwnerHandoff(accountKey: string, owner: ResumeOwner): void {
  writeJson(ownerHandoffKey(accountKey, owner.ownerId), {
    ownerId: owner.ownerId,
    instanceId: owner.instanceId,
    expiresAt: Date.now() + OWNER_HANDOFF_TTL_MS,
  }, "owner-handoff");
}

function ownerRegistryKey(accountKey: string, ownerId: string): string {
  return `${accountKey.length}:${accountKey}.${ownerId.length}:${ownerId}`;
}

function ownerRegistrationKey(accountKey: string, owner: ResumeOwner): string {
  return `${ownerRegistryKey(accountKey, owner.ownerId)}.${owner.instanceId.length}:${owner.instanceId}`;
}

function ownerLeaseKey(accountKey: string, ownerId: string): string {
  return `${OWNER_LEASE_PREFIX}.${ownerRegistryKey(accountKey, ownerId)}`;
}

function ownerHandoffKey(accountKey: string, ownerId: string): string {
  return `${OWNER_HANDOFF_PREFIX}.${ownerRegistryKey(accountKey, ownerId)}`;
}

function readOwnerLease(key: string): OwnerLease | null {
  return readJson<OwnerLease>(key, isOwnerLease, "owner-lease");
}

function readOwnerHandoff(key: string): OwnerHandoff | null {
  return readJson<OwnerHandoff>(key, isOwnerHandoff, "owner-handoff");
}

function removeOwnerLeaseIfOwned(accountKey: string, owner: ResumeOwner): void {
  removeJsonIfOwned(
    ownerLeaseKey(accountKey, owner.ownerId),
    isOwnerLease,
    owner,
    "owner-lease",
  );
}

function removeOwnerHandoffIfOwned(accountKey: string, owner: ResumeOwner): void {
  removeJsonIfOwned(
    ownerHandoffKey(accountKey, owner.ownerId),
    isOwnerHandoff,
    owner,
    "owner-handoff",
  );
}

function removeSmEnvelopeIfOwned(key: string, owner: ResumeOwner): void {
  removeJsonIfOwned(key, isPersistedSmEnvelope, owner, "sm");
}

function removeSmConsumedMarkerIfOwned(key: string, owner: ResumeOwner): void {
  removeJsonIfOwned(key, isPersistedSmConsumedMarker, owner, "sm-consumed");
}

function removeJsonIfOwned<T extends { ownerId?: string; instanceId?: string; ownerInstanceId?: string }>(
  key: string,
  validate: (value: unknown) => value is T,
  owner: ResumeOwner,
  kind: string,
): void {
  const s = storage();
  if (!s) return;
  try {
    const raw = s.getItem(key);
    if (!raw) return;
    const parsed: unknown = JSON.parse(raw);
    if (!validate(parsed) || parsed.ownerId !== owner.ownerId) return;
    const instanceId = "ownerInstanceId" in (parsed as object)
      ? (parsed as { ownerInstanceId?: unknown }).ownerInstanceId
      : (parsed as { instanceId?: unknown }).instanceId;
    if (instanceId !== owner.instanceId) return;
    // localStorage operations are synchronous, but re-read before deletion so a
    // re-entrant storage implementation cannot let a stale owner erase a
    // replacement's lease, handoff, or SM tail.
    if (s.getItem(key) === raw) s.removeItem(key);
  } catch (err) {
    reportError("storage.write", err, {
      recoverable: true,
      detail: `resume-persistence compare-delete failed (${kind})`,
      storage_area: kind,
    });
  }
}

/** Test-only visibility for proving owner timers and in-memory slots drain. */
export function resumeOwnerLifecycleSnapshotForTests(): {
  registrations: number;
  activeTimers: number;
  ownerInstances: number;
} {
  return {
    registrations: liveOwnerHeartbeats.size,
    activeTimers: liveOwnerHeartbeats.size,
    ownerInstances: liveOwnerInstances.size,
  };
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
      storage_area: kind,
    });
    return null;
  }
}

function storageKeysWithPrefix(prefix: string): string[] {
  const s = storage();
  if (!s) return [];
  const keys: string[] = [];
  try {
    for (let index = 0; index < s.length; index += 1) {
      const key = s.key(index);
      if (key?.startsWith(prefix)) keys.push(key);
    }
  } catch (err) {
    reportError("storage.read", err, {
      recoverable: true,
      detail: "resume-persistence shard enumeration failed (catchup)",
      storage_area: "catchup",
    });
  }
  return keys;
}

function readCatchupShards(prefix: string): PersistedReconnectCatchup | null {
  const snapshots = storageKeysWithPrefix(`${prefix}.`)
    .map((key) => readJson<PersistedReconnectCatchup>(key, isPersistedReconnectCatchup, "catchup"))
    .filter((snapshot): snapshot is PersistedReconnectCatchup => !!snapshot);
  if (snapshots.length === 0) return null;
  return {
    dmLastSeen: snapshots.flatMap((snapshot) => snapshot.dmLastSeen),
    roomLastSeen: snapshots.flatMap((snapshot) => snapshot.roomLastSeen),
  };
}

function removeKeysWithPrefix(prefix: string, kind: string): void {
  for (const key of storageKeysWithPrefix(prefix)) removeKey(key, kind);
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
      storage_area: kind,
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
      storage_area: kind,
    });
  }
}

function isPersistedSeenCursor(value: unknown): value is PersistedSeenCursor {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  if (typeof candidate.timestamp !== "string") return false;
  if (candidate.scope !== undefined && candidate.scope !== "account" && candidate.scope !== "muc-occupant") return false;
  if (candidate.archiveId !== undefined && typeof candidate.archiveId !== "string") return false;
  if (candidate.archiveTimestamp !== undefined && typeof candidate.archiveTimestamp !== "string") return false;
  if (candidate.archiveSeenIds !== undefined) {
    if (!Array.isArray(candidate.archiveSeenIds)) return false;
    if (!candidate.archiveSeenIds.every((id) => typeof id === "string")) return false;
  }
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

function isPersistedAutoJoinBlockArray(value: unknown): value is PersistedAutoJoinBlock[] {
  return Array.isArray(value) && value.every((item) => {
    if (!item || typeof item !== "object") return false;
    const candidate = item as Record<string, unknown>;
    return typeof candidate.roomJid === "string"
      && (
        candidate.condition === "registration-required"
        || candidate.condition === "forbidden"
      )
      && (
        candidate.catalogFingerprint === undefined
        || candidate.catalogFingerprint === null
        || typeof candidate.catalogFingerprint === "string"
      )
      && (
        candidate.catalogFingerprintFields === undefined
        || (
          typeof candidate.catalogFingerprint === "string"
          && isRoomCatalogFingerprintFieldArray(
            candidate.catalogFingerprintFields,
          )
        )
      );
  });
}

function isRoomCatalogFingerprintFieldArray(
  value: unknown,
): value is RoomCatalogFingerprintField[] {
  return Array.isArray(value)
    && value.every(
      (field) =>
        typeof field === "string"
        && ROOM_CATALOG_FINGERPRINT_FIELDS.includes(
          field as RoomCatalogFingerprintField,
        ),
    );
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

function isPersistedSmConsumedMarker(value: unknown): value is PersistedSmConsumedMarker {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  return typeof candidate.marker === "string"
    && Number.isFinite(candidate.savedAt)
    && (candidate.ownerId === undefined || typeof candidate.ownerId === "string")
    && (candidate.ownerInstanceId === undefined || typeof candidate.ownerInstanceId === "string");
}

function isU32(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= 0xFFFF_FFFF;
}

function isPositiveU32(value: unknown): value is number {
  return isU32(value) && value > 0;
}

function normalizeRoomJid(roomJid: string): string {
  return bareJidKey(roomJid);
}

function isPersistedSmEnvelope(value: unknown): value is PersistedSmEnvelope {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  // The pre-entry format cannot reconstruct original send timestamps. Never
  // resume its counters without its sender-owned tail: use the durable browser
  // queue on a fresh stream instead.
  if (Array.isArray(candidate.unhandledOutboundStanzas) && candidate.unhandledOutboundStanzas.length > 0) {
    return false;
  }
  return (
    typeof candidate.previd === "string"
    && isU32(candidate.inboundH)
    && isU32(candidate.outboundH)
    && (candidate.maxResumeSeconds === undefined || isPositiveU32(candidate.maxResumeSeconds))
    && (candidate.unhandledOutboundEntries === undefined || isPersistedUnhandledOutboundEntries(candidate.unhandledOutboundEntries))
    && (candidate.resource === undefined || typeof candidate.resource === "string")
    && (candidate.ownerId === undefined || typeof candidate.ownerId === "string")
    && (candidate.ownerInstanceId === undefined || typeof candidate.ownerInstanceId === "string")
    && (candidate.claimId === undefined || typeof candidate.claimId === "string")
    && Number.isFinite(candidate.savedAt)
  );
}

function isPersistedUnhandledOutboundEntries(value: unknown): value is Array<{ xml: string; sentAt: string }> {
  return Array.isArray(value) && value.every((entry) => {
    if (!entry || typeof entry !== "object") return false;
    const candidate = entry as Record<string, unknown>;
    return typeof candidate.xml === "string" && typeof candidate.sentAt === "string";
  });
}
