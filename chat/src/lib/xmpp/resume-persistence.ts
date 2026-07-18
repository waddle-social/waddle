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
 *     outbound stanza counts, advertised resume window, and the authoritative
 *     ordered outbound-entry array. Every entry carries a typed XML token tree
 *     and numeric original send instant so `doConnect` can restore sender
 *     responsibility after a full reload without reparsing raw XML.
 *
 * The shape mirrors `outbound-queue-store.ts` (same `waddle.chat.*`
 * prefix family, same per-account key namespacing, same defensive
 * read/write error handling around localStorage availability and
 * quota) so the storage surface stays uniform.
 */

import { reportError } from "@/lib/telemetry";
import type {
  DurableOutboundStore,
  DurableOutcome,
  DurableSmEnvelope,
  OutboundOwnerActivation,
  OutboundOwnerContext,
  OutboundOwnerHint,
  PagehideHandoffResult,
} from "@/lib/xmpp-runtime/durable-contract";
import { IndexedDbDurableOutboundStore } from "@/lib/xmpp-runtime-durable-store";

const CATCHUP_PREFIX = "waddle.chat.resume-cursors";
const SM_PREFIX = "waddle.chat.sm-resume";
const JOINED_ROOMS_PREFIX = "waddle.chat.joined-rooms";
const OWNER_KEY = `${SM_PREFIX}.owner`;
const OWNER_HANDOFF_TOKEN_KEY = `${SM_PREFIX}.owner-handoff-token`;

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

export type {
  PersistedSmResumeState,
  XmppResumeEntry,
  XmppResumeStanza,
  XmppResumeStanzaKind,
} from "./sm-resume-types";
import type {
  PersistedSmResumeState,
} from "./sm-resume-types";
import { cloneSmResumeState } from "./sm-resume-types";

type ResumeOwner = {
  ownerId: string;
  instanceId: XmppLifecycleId;
  explicit: boolean;
  handoffToken?: string;
};

/**
 * Immutable UUID for one browser XMPP client/persistence construction.
 *
 * The wrapper prevents a tab/session owner id (which intentionally survives a
 * reload) from being confused with the live process incarnation that must
 * never be reused by a replacement client.
 */
export class XmppLifecycleId {
  private constructor(readonly value: string) {}

  static create(): XmppLifecycleId {
    return new XmppLifecycleId(randomClaimId());
  }

  static explicitForTest(value: string): XmppLifecycleId {
    return new XmppLifecycleId(value);
  }
}

/**
 * Fallback resumption window for older persisted PODs that predate
 * `maxResumeSeconds`. This mirrors Waddle server's default XEP-0198
 * max, so old snapshots fail closed instead of lingering for hours.
 */
const DEFAULT_SM_MAX_RESUME_SECONDS = 300;
const SM_SAVED_AT_FUTURE_SKEW_MS = 60_000;
function defaultDurableSmStore(): DurableOutboundStore {
  return new IndexedDbDurableOutboundStore();
}

function cloneSmState(state: PersistedSmResumeState): PersistedSmResumeState {
  return cloneSmResumeState(state);
}

function cloneSmEnvelope(envelope: DurableSmEnvelope): DurableSmEnvelope {
  return {
    ...envelope,
    state: cloneSmState(envelope.state),
  };
}

function clonePagehideHandoff(
  receipt: PagehideHandoffResult,
): PagehideHandoffResult {
  return {
    handoff: { ...receipt.handoff },
    smVersion: receipt.smVersion,
  };
}

export interface ResumePersistence {
  /** Exact live incarnation shared by SM and outbound queue ownership. */
  readonly lifecycleId: XmppLifecycleId;
  /**
   * The unified store that owns both outbound claims and this persistence's
   * SM snapshot. BrowserXmppClient adopts it when a persistence adapter is
   * injected so the two sides cannot accidentally fence one another.
   */
  readonly durableRuntimeStore?: DurableOutboundStore;
  loadCatchup(): PersistedReconnectCatchup | null;
  saveCatchup(snapshot: PersistedReconnectCatchup): void;
  clearCatchup(): void;
  loadSm(): Promise<DurableOutcome<PersistedSmResumeState | null>>;
  consumeSm(): Promise<DurableOutcome<PersistedSmResumeState | null>>;
  saveSm(state: PersistedSmResumeState): Promise<DurableOutcome<void>>;
  clearSm(): Promise<DurableOutcome<boolean>>;
  outboundOwnerHint(): OutboundOwnerHint;
  acceptOutboundOwner(activation: OutboundOwnerActivation): void;
  preparePagehideHandoff(
    state: PersistedSmResumeState | null,
  ): Promise<DurableOutcome<PagehideHandoffResult>>;
  publishPagehideHandoff(receipt: PagehideHandoffResult): void;
  reclaimPagehideOwnership(): Promise<DurableOutcome<void>>;
  loadJoinedRooms(): string[];
  saveJoinedRooms(roomJids: readonly string[]): void;
  clearJoinedRooms(): void;
}

/** No-op persistence — used in tests / non-browser contexts. */
export const nullResumePersistence: ResumePersistence = {
  lifecycleId: XmppLifecycleId.explicitForTest("null-persistence"),
  loadCatchup: () => null,
  saveCatchup: () => undefined,
  clearCatchup: () => undefined,
  loadSm: async () => ({ kind: "committed", value: null }),
  consumeSm: async () => ({ kind: "committed", value: null }),
  saveSm: async () => ({ kind: "committed", value: undefined }),
  clearSm: async () => ({ kind: "committed", value: false }),
  outboundOwnerHint: () => ({ ownerId: "null-owner", ownerInstanceId: "null-instance" }),
  acceptOutboundOwner: () => undefined,
  preparePagehideHandoff: async () => ({
    kind: "committed",
    value: {
      handoff: {
        token: "null-handoff",
        expiresAt: 0,
        authorityEpoch: 0,
        ownerGeneration: 0,
      },
      smVersion: 0,
    },
  }),
  publishPagehideHandoff: () => undefined,
  reclaimPagehideOwnership: async () => ({ kind: "committed", value: undefined }),
  loadJoinedRooms: () => [],
  saveJoinedRooms: () => undefined,
  clearJoinedRooms: () => undefined,
};

export function createLocalStorageResumePersistence(
  accountKey: string,
  ownerId?: string,
  durableStore: DurableOutboundStore = defaultDurableSmStore(),
  lifecycleId: XmppLifecycleId = XmppLifecycleId.create(),
): ResumePersistence {
  const owner = ownerId
    ? explicitResumeOwner(ownerId, lifecycleId)
    : resumeOwner(lifecycleId);
  let resolvedOwner: OutboundOwnerContext | null = null;
  let atomicallyConsumedHandoff: DurableSmEnvelope | null = null;
  let preparedHandoff: PagehideHandoffResult | null = null;
  let smVersion: number | null | undefined;
  // Length-prefix the account segment so prefix enumeration cannot make
  // `alice@example.com` consume `alice@example.com.evil` shards.
  const catchupKeyPrefix = `${CATCHUP_PREFIX}.${accountKey.length}:${accountKey}`;
  const ownerStorageFence = () => resolvedOwner
    ? `${resolvedOwner.ownerId}.g${resolvedOwner.ownerGeneration}`
    : `${owner.ownerId}.pending-${lifecycleId.value}`;
  const catchupKey = () => `${catchupKeyPrefix}.${ownerStorageFence()}`;
  const joinedRoomsKeyPrefix = () => (
    `${JOINED_ROOMS_PREFIX}.${accountKey.length}:${accountKey}.${owner.ownerId}`
  );
  const joinedRoomsKey = () => (
    `${JOINED_ROOMS_PREFIX}.${accountKey.length}:${accountKey}.${ownerStorageFence()}`
  );

  const ensureOwner = async (): Promise<DurableOutcome<OutboundOwnerContext>> => {
    if (resolvedOwner) return { kind: "committed", value: resolvedOwner };
    const outcome = await durableStore.claimOwner(accountKey, {
      ownerId: owner.ownerId,
      ownerInstanceId: owner.instanceId.value,
      ...(owner.handoffToken ? { handoffToken: owner.handoffToken } : {}),
    });
    if (outcome.kind === "failed") return outcome;
    const activation = outcome.value;
    if (activation.fence.ownerInstanceId !== owner.instanceId.value) {
      return lifecycleFenceFailure();
    }
    resolvedOwner = activation.fence;
    if (activation.handoffSm) {
      atomicallyConsumedHandoff = cloneSmEnvelope(activation.handoffSm);
      smVersion = activation.handoffSm.version;
    }
    owner.ownerId = activation.fence.ownerId;
    delete owner.handoffToken;
    writeOwnerSession(owner);
    return { kind: "committed", value: activation.fence };
  };

  const loadEnvelope = async (
    currentOwner: OutboundOwnerContext,
  ): Promise<DurableOutcome<DurableSmEnvelope | null>> => {
    const outcome = await durableStore.loadSm(currentOwner);
    if (outcome.kind === "failed") return outcome;
    if (outcome.value.kind === "fenced") return smFenceFailure("load");
    const envelope = outcome.value.envelope;
    smVersion = outcome.value.version;
    return { kind: "committed", value: envelope };
  };

  const ensureSmVersion = async (
    currentOwner: OutboundOwnerContext,
  ): Promise<DurableOutcome<number | null>> => {
    if (smVersion !== undefined) return { kind: "committed", value: smVersion };
    const outcome = await loadEnvelope(currentOwner);
    if (outcome.kind === "failed") return outcome;
    return { kind: "committed", value: smVersion ?? null };
  };

  return {
    lifecycleId,
    durableRuntimeStore: durableStore,
    loadCatchup() {
      return readCatchupShards(catchupKeyPrefix);
    },
    saveCatchup(snapshot) {
      writeJson(catchupKey(), snapshot, "catchup");
    },
    clearCatchup() {
      removeKey(catchupKey(), "catchup");
    },
    async loadSm() {
      const ownerOutcome = await ensureOwner();
      if (ownerOutcome.kind === "failed") return ownerOutcome;
      if (atomicallyConsumedHandoff) {
        const envelope = atomicallyConsumedHandoff;
        if (
          envelope.ownerId === owner.ownerId
          && !smEnvelopeExpired(envelope)
        ) {
          return { kind: "committed", value: cloneSmState(envelope.state) };
        }
        return { kind: "committed", value: null };
      }
      const outcome = await loadEnvelope(ownerOutcome.value);
      if (outcome.kind === "failed") return outcome;
      const envelope = outcome.value;
      if (
        !envelope
        || envelope.ownerId !== owner.ownerId
        || envelope.consumed
        || smEnvelopeExpired(envelope)
      ) {
        return { kind: "committed", value: null };
      }
      return { kind: "committed", value: cloneSmState(envelope.state) };
    },
    async consumeSm() {
      const ownerOutcome = await ensureOwner();
      if (ownerOutcome.kind === "failed") return ownerOutcome;
      if (atomicallyConsumedHandoff) {
        const envelope = atomicallyConsumedHandoff;
        atomicallyConsumedHandoff = null;
        return {
          kind: "committed",
          value: envelope.ownerId === owner.ownerId
            && !smEnvelopeExpired(envelope)
            ? cloneSmState(envelope.state)
            : null,
        };
      }
      const loaded = await loadEnvelope(ownerOutcome.value);
      if (loaded.kind === "failed") return loaded;
      const envelope = loaded.value;
      if (
        !envelope
        || envelope.consumed
        || smEnvelopeExpired(envelope)
      ) {
        return { kind: "committed", value: null };
      }
      const outcome = await durableStore.consumeSm(
        ownerOutcome.value,
        envelope.version,
        (envelope) => !smEnvelopeExpired(envelope),
      );
      if (outcome.kind === "failed") return outcome;
      if (outcome.value.kind === "fenced") return smFenceFailure("consume");
      if (outcome.value.kind === "stale") return smStaleFailure("consume");
      smVersion = outcome.value.value?.version ?? envelope.version;
      return {
        kind: "committed",
        value: outcome.value.value ? cloneSmState(outcome.value.value.state) : null,
      };
    },
    async saveSm(state) {
      const ownerOutcome = await ensureOwner();
      if (ownerOutcome.kind === "failed") return ownerOutcome;
      const versionOutcome = await ensureSmVersion(ownerOutcome.value);
      if (versionOutcome.kind === "failed") return versionOutcome;
      const outcome = await durableStore.saveSm(
        ownerOutcome.value,
        versionOutcome.value,
        cloneSmState(state),
        Date.now(),
      );
      if (outcome.kind === "failed") return outcome;
      if (outcome.value.kind === "fenced") return smFenceFailure("save");
      if (outcome.value.kind === "stale") return smStaleFailure("save");
      smVersion = outcome.value.value.version;
      return { kind: "committed", value: undefined };
    },
    async clearSm() {
      const ownerOutcome = await ensureOwner();
      if (ownerOutcome.kind === "failed") return ownerOutcome;
      const versionOutcome = await ensureSmVersion(ownerOutcome.value);
      if (versionOutcome.kind === "failed") return versionOutcome;
      const outcome = await durableStore.clearSm(
        ownerOutcome.value,
        versionOutcome.value,
      );
      if (outcome.kind === "failed") return outcome;
      if (outcome.value.kind === "fenced") return smFenceFailure("clear");
      if (outcome.value.kind === "stale") return smStaleFailure("clear");
      smVersion = outcome.value.value.version;
      return { kind: "committed", value: outcome.value.value.cleared };
    },
    outboundOwnerHint() {
      return {
        ownerId: owner.ownerId,
        ownerInstanceId: owner.instanceId.value,
        ...(owner.handoffToken ? { handoffToken: owner.handoffToken } : {}),
      };
    },
    acceptOutboundOwner(activation) {
      const resolved = activation.fence;
      if (resolved.ownerInstanceId !== owner.instanceId.value) {
        throw new DOMException(
          "Outbound owner lifecycle changed during activation",
          "AbortError",
        );
      }
      if (
        resolvedOwner
        && (
          resolvedOwner.ownerId !== resolved.ownerId
          || resolvedOwner.ownerGeneration !== resolved.ownerGeneration
          || resolvedOwner.authorityEpoch !== resolved.authorityEpoch
        )
      ) {
        smVersion = undefined;
        preparedHandoff = null;
      }
      resolvedOwner = resolved;
      if (activation.handoffSm) {
        atomicallyConsumedHandoff = cloneSmEnvelope(activation.handoffSm);
        smVersion = activation.handoffSm.version;
      }
      owner.ownerId = resolved.ownerId;
      delete owner.handoffToken;
      writeOwnerSession(owner);
    },
    async preparePagehideHandoff(state) {
      const ownerOutcome = await ensureOwner();
      if (ownerOutcome.kind === "failed") return ownerOutcome;
      const versionOutcome = await ensureSmVersion(ownerOutcome.value);
      if (versionOutcome.kind === "failed") return versionOutcome;
      const token = randomClaimId();
      const prepared = await durableStore.preparePagehideHandoff(
        ownerOutcome.value,
        versionOutcome.value,
        token,
        state ? cloneSmState(state) : null,
      );
      if (prepared.kind === "failed") return prepared;
      if (prepared.value.kind === "fenced") return smFenceFailure("prepare-handoff");
      if (prepared.value.kind === "stale") return smStaleFailure("prepare-handoff");
      smVersion = prepared.value.value.smVersion;
      preparedHandoff = clonePagehideHandoff(prepared.value.value);
      return {
        kind: "committed",
        value: clonePagehideHandoff(prepared.value.value),
      };
    },
    publishPagehideHandoff(receipt) {
      if (
        !preparedHandoff
        || preparedHandoff.handoff.token !== receipt.handoff.token
        || preparedHandoff.smVersion !== receipt.smVersion
      ) {
        throw new DOMException(
          "Pagehide handoff receipt changed before publication",
          "AbortError",
        );
      }
      owner.handoffToken = receipt.handoff.token;
      // Publish only after the exact durable owner+SM transaction committed.
      writeOwnerSession(owner);
    },
    async reclaimPagehideOwnership() {
      const current = preparedHandoff;
      if (!current || !resolvedOwner) {
        return { kind: "committed", value: undefined };
      }
      const outcome = await durableStore.cancelOwnerHandoff(
        resolvedOwner,
        current.handoff.token,
        current.smVersion,
      );
      if (outcome.kind === "failed") return outcome;
      if (outcome.value.kind === "fenced") {
        return smFenceFailure("cancel-handoff");
      }
      if (outcome.value.kind === "stale") {
        return smStaleFailure("cancel-handoff");
      }
      preparedHandoff = null;
      if (owner.handoffToken === current.handoff.token) {
        delete owner.handoffToken;
        writeOwnerSession(owner);
      }
      return { kind: "committed", value: undefined };
    },
    loadJoinedRooms() {
      const stored = storageKeysWithPrefix(`${joinedRoomsKeyPrefix()}.`)
        .flatMap((key) => (
          readJson<string[]>(key, isStringArray, "joined-rooms") ?? []
        ));
      return [...new Set(stored.map(normalizeRoomJid).filter(Boolean))];
    },
    saveJoinedRooms(roomJids) {
      writeJson(
        joinedRoomsKey(),
        [...new Set(roomJids.map(normalizeRoomJid).filter(Boolean))],
        "joined-rooms",
      );
    },
    clearJoinedRooms() {
      removeKey(joinedRoomsKey(), "joined-rooms");
    },
  };
}

function smFenceFailure<T>(operation: string): DurableOutcome<T> {
  return {
    kind: "failed",
    reason: "aborted",
    cause: new DOMException(`SM owner fenced during ${operation}`, "AbortError"),
  };
}

function lifecycleFenceFailure<T>(): DurableOutcome<T> {
  return {
    kind: "failed",
    reason: "aborted",
    cause: new DOMException(
      "Outbound owner lifecycle changed during activation",
      "AbortError",
    ),
  };
}

function smStaleFailure<T>(operation: string): DurableOutcome<T> {
  return {
    kind: "failed",
    reason: "aborted",
    cause: new DOMException(`SM snapshot changed during ${operation}`, "AbortError"),
  };
}

function reportSmOwnerFailure(operation: string, cause: unknown): void {
  reportError("storage.write", cause, {
    recoverable: false,
    detail: `SM owner ${operation} failed`,
    storage_area: "sm-resume",
  });
}

function randomClaimId(): string {
  return globalThis.crypto?.randomUUID?.() ?? Math.random().toString(36).slice(2);
}

function smEnvelopeExpired(envelope: DurableSmEnvelope): boolean {
  const maxResumeSeconds = envelope.state.maxResumeSeconds ?? DEFAULT_SM_MAX_RESUME_SECONDS;
  const ageMs = Date.now() - envelope.savedAt;
  if (ageMs < -SM_SAVED_AT_FUTURE_SKEW_MS) return true;
  return ageMs > maxResumeSeconds * 1000;
}

function explicitResumeOwner(
  ownerId: string,
  lifecycleId: XmppLifecycleId,
): ResumeOwner {
  return {
    ownerId,
    instanceId: lifecycleId,
    explicit: true,
  };
}

function resumeOwner(lifecycleId: XmppLifecycleId): ResumeOwner {
  const s = sessionStorageForOwner();
  if (!s) {
    const ownerId = randomClaimId();
    return { ownerId, instanceId: lifecycleId, explicit: false };
  }
  try {
    const ownerId = s.getItem(OWNER_KEY) || randomClaimId();
    const owner: ResumeOwner = {
      ownerId,
      instanceId: lifecycleId,
      explicit: false,
      ...(s.getItem(OWNER_HANDOFF_TOKEN_KEY)
        ? { handoffToken: s.getItem(OWNER_HANDOFF_TOKEN_KEY)! }
        : {}),
    };
    writeOwnerSession(owner);
    return owner;
  } catch {
    const ownerId = randomClaimId();
    return { ownerId, instanceId: lifecycleId, explicit: false };
  }
}

function writeOwnerSession(owner: ResumeOwner): void {
  if (owner.explicit) return;
  const s = sessionStorageForOwner();
  if (!s) return;
  try {
    s.setItem(OWNER_KEY, owner.ownerId);
    if (owner.handoffToken) {
      s.setItem(OWNER_HANDOFF_TOKEN_KEY, owner.handoffToken);
    } else {
      s.removeItem(OWNER_HANDOFF_TOKEN_KEY);
    }
  } catch {
    // IndexedDB owner activation fails closed; sessionStorage is only a hint.
  }
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

function normalizeRoomJid(roomJid: string): string {
  return roomJid.split("/")[0]?.trim().toLowerCase() ?? "";
}
