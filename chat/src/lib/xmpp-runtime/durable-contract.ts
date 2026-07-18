import type { PersistedQueuedMessage } from "../outbound-queue-store";
import { bareJidKey } from "../xmpp/jid";
import type { PersistedSmResumeState } from "../xmpp/sm-resume-types";

export const OUTBOUND_CLAIM_LEASE_MS = 45_000;
export const SM_SNAPSHOT_RETENTION_MS = 8 * 24 * 60 * 60 * 1_000;
export const RETAINED_PREDECESSOR_LIMIT = 64;

export type DurableFailureReason =
  | "unavailable"
  | "quota"
  | "security"
  | "capacity"
  | "aborted";

export type DurableOutcome<T> =
  | { kind: "committed"; value: T }
  | { kind: "failed"; reason: DurableFailureReason; cause?: unknown };

export class OutboundPersistenceError extends Error {
  constructor(
    readonly operation: string,
    readonly reason: DurableFailureReason,
    readonly cause?: unknown,
  ) {
    super(`Outbound persistence ${operation} failed: ${reason}`);
    this.name = "OutboundPersistenceError";
  }
}

export class DurablePredecessorCapacityError extends Error {
  constructor(readonly limit = RETAINED_PREDECESSOR_LIMIT) {
    super(`Durable predecessor capacity ${limit} is exhausted`);
    this.name = "DurablePredecessorCapacityError";
  }
}

export function committedOrThrow<T>(operation: string, outcome: DurableOutcome<T>): T {
  if (outcome.kind === "committed") return outcome.value;
  throw new OutboundPersistenceError(operation, outcome.reason, outcome.cause);
}

/** Minimal fence copied into claims, intents, and SM mutations. */
export type OutboundOwnerFence = {
  accountKey: string;
  ownerId: string;
  ownerInstanceId: string;
  ownerGeneration: number;
  authorityEpoch: number;
};

/**
 * Activation-only payload. A pagehide SM snapshot may be large and must never
 * be spread into a per-message claim or terminal intent.
 */
export type OutboundOwnerActivation = {
  fence: OutboundOwnerFence;
  /**
   * Present only on the one successor that atomically consumed a valid
   * pagehide handoff. It is already marked consumed in durable storage.
   */
  handoffSm?: DurableSmEnvelope;
};

export type OutboundOwnerContext = OutboundOwnerFence;

export type OutboundOwnerHint = {
  ownerId: string;
  ownerInstanceId: string;
  handoffToken?: string;
};

export type OutboundOwnerHandoff = {
  token: string;
  expiresAt: number;
  authorityEpoch: number;
  ownerGeneration: number;
};

export type OutboundClaimPhase =
  | "sending"
  | "resume-replay"
  | "fresh-fallback";

export type OutboundClaimRequest = OutboundOwnerContext & {
  connectionGeneration: number;
  claimId: string;
  phase: OutboundClaimPhase;
};

export type OutboundRowIdentity = {
  accountKey: string;
  messageId: string;
  incarnation: string;
  payloadDigest: string;
};

export type OutboundClaim = OutboundClaimRequest & {
  rowIncarnation: string;
  payloadDigest: string;
  leaseUntil: number;
};

export type OutboundLane =
  | { kind: "direct" }
  | { kind: "room"; roomJid: string };

export type RoomOutboundLane = Extract<OutboundLane, { kind: "room" }>;

export type DurableOutboundEntryState =
  | { kind: "ready" }
  | { kind: "claimed"; claim: OutboundClaim }
  | { kind: "terminal"; intentId: string };

export type DurableOutboundEntry = {
  identity: OutboundRowIdentity;
  lane: OutboundLane;
  message: PersistedQueuedMessage;
  state: DurableOutboundEntryState;
};

export type DurableOutboundScan = {
  entries: DurableOutboundEntry[];
  pruned: OutboundRowIdentity[];
  revision: number;
};

export type OutboundPersistResult =
  | { kind: "inserted"; entry: DurableOutboundEntry }
  | { kind: "existing"; entry: DurableOutboundEntry }
  | {
      kind: "conflict";
      messageId: string;
      existingPayloadDigest: string;
      attemptedPayloadDigest: string;
    };

type OutboundLaneBlocker = {
  identity: OutboundRowIdentity;
  state: DurableOutboundEntryState["kind"];
  leaseUntil?: number;
};

export type OutboundPersistLaneHeadResult =
  | { kind: "claimed"; entry: DurableOutboundEntry; claim: OutboundClaim }
  | {
      kind: "queued";
      entry: DurableOutboundEntry;
      blocker: OutboundLaneBlocker;
    }
  | { kind: "busy"; entry: DurableOutboundEntry; leaseUntil: number }
  | { kind: "terminal"; entry: DurableOutboundEntry }
  | { kind: "fenced" }
  | Extract<OutboundPersistResult, { kind: "conflict" }>;

export type OutboundClaimHeadResult =
  | { kind: "claimed"; entry: DurableOutboundEntry; claim: OutboundClaim }
  | { kind: "busy"; messageId: string; leaseUntil: number }
  | { kind: "missing" }
  | { kind: "terminal"; messageId: string }
  | { kind: "fenced" };

export type OutboundRenewResult =
  | { kind: "renewed"; claim: OutboundClaim }
  | { kind: "missing" }
  | { kind: "fenced" };

export type OutboundReleaseResult =
  | { kind: "released" }
  | { kind: "missing" }
  | { kind: "fenced" };

export type ResumeClaimReconciliation =
  | {
      kind: "reconciled";
      claims: Array<{ messageId: string; claim: OutboundClaim }>;
      releasedIds: string[];
      blockedIds: string[];
      terminalIds: string[];
      missingIds: string[];
    }
  | { kind: "fenced" };

export type OutboundTerminalKind =
  | "ack"
  | "native-failure"
  | "nonretryable-delete";

export type OutboundTerminalIntent = {
  intentId: string;
  accountKey: string;
  identity: OutboundRowIdentity;
  kind: OutboundTerminalKind;
  expected: OutboundClaim;
  recordedAt: number;
};

export type OutboundTerminalRecordResult =
  | { kind: "recorded"; intent: OutboundTerminalIntent }
  | { kind: "missing" }
  | { kind: "stale" }
  | { kind: "fenced" };

export type OutboundTerminalApplyResult =
  | { kind: "acked"; identity: OutboundRowIdentity }
  | { kind: "removed"; identity: OutboundRowIdentity }
  | { kind: "released"; identity: OutboundRowIdentity }
  | { kind: "fallback"; identity: OutboundRowIdentity; claim: OutboundClaim }
  | { kind: "missing" }
  | { kind: "stale" }
  | { kind: "fenced" };

export type DurableSmEnvelope = {
  accountKey: string;
  ownerId: string;
  ownerGeneration: number;
  authorityEpoch: number;
  version: number;
  state: PersistedSmResumeState;
  savedAt: number;
  consumed: boolean;
};

export type DurableSmLoadResult =
  | { kind: "loaded"; envelope: DurableSmEnvelope | null; version: number | null }
  | { kind: "fenced" };

export type DurableSmClearResult = {
  cleared: boolean;
  version: number;
};

export type DurableSmMutationResult<T> =
  | { kind: "applied"; value: T }
  | { kind: "stale"; actualVersion: number | null }
  | { kind: "fenced" };

export type PagehideHandoffResult = {
  handoff: OutboundOwnerHandoff;
  smVersion: number;
};

export type PagehideHandoffCancelResult =
  | { kind: "applied"; cancelled: boolean }
  | {
      kind: "stale";
      actualToken: string | null;
      actualSmVersion: number | null;
    }
  | { kind: "fenced" };

interface DurableSmResumeStore {
  loadSm(
    owner: OutboundOwnerContext,
  ): Promise<DurableOutcome<DurableSmLoadResult>>;
  consumeSm(
    owner: OutboundOwnerContext,
    expectedVersion: number | null,
    usable: (envelope: DurableSmEnvelope) => boolean,
  ): Promise<DurableOutcome<DurableSmMutationResult<DurableSmEnvelope | null>>>;
  saveSm(
    owner: OutboundOwnerContext,
    expectedVersion: number | null,
    state: PersistedSmResumeState,
    _savedAt: number,
  ): Promise<DurableOutcome<DurableSmMutationResult<DurableSmEnvelope>>>;
  clearSm(
    owner: OutboundOwnerContext,
    expectedVersion: number | null,
  ): Promise<DurableOutcome<DurableSmMutationResult<DurableSmClearResult>>>;
}

export interface DurableOutboundStore extends DurableSmResumeStore {
  revision(accountKey: string): Promise<DurableOutcome<number>>;
  list(accountKey: string): Promise<DurableOutcome<PersistedQueuedMessage[]>>;
  scanAndPrune(
    accountKey: string,
    cutoff: number,
  ): Promise<DurableOutcome<DurableOutboundScan>>;
  persistReady(
    accountKey: string,
    message: PersistedQueuedMessage,
  ): Promise<DurableOutcome<OutboundPersistResult>>;
  persistAndClaimLaneHead(
    accountKey: string,
    message: PersistedQueuedMessage,
    claim: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundPersistLaneHeadResult>>;
  claimHead(
    accountKey: string,
    lane: OutboundLane,
    claim: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundClaimHeadResult>>;
  renew(
    identity: OutboundRowIdentity,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<OutboundRenewResult>>;
  release(
    identity: OutboundRowIdentity,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<OutboundReleaseResult>>;
  reconcileResumeClaims(
    owner: OutboundOwnerContext,
    connectionGeneration: number,
    authoritativeMessageIds: readonly string[] | null,
    phase: Extract<OutboundClaimPhase, "resume-replay" | "fresh-fallback">,
  ): Promise<DurableOutcome<ResumeClaimReconciliation>>;
  releaseForFreshSession(
    owner: OutboundOwnerContext,
    connectionGeneration: number,
  ): Promise<DurableOutcome<string[] | null>>;
  listTerminal(
    accountKey: string,
  ): Promise<DurableOutcome<OutboundTerminalIntent[]>>;
  recordTerminal(
    identity: OutboundRowIdentity,
    kind: OutboundTerminalKind,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<OutboundTerminalRecordResult>>;
  applyTerminal(
    executor: OutboundOwnerContext,
    intent: OutboundTerminalIntent,
  ): Promise<DurableOutcome<OutboundTerminalApplyResult>>;
  claimOwner(
    accountKey: string,
    hint: OutboundOwnerHint,
  ): Promise<DurableOutcome<OutboundOwnerActivation>>;
  renewOwner(
    owner: OutboundOwnerContext,
  ): Promise<DurableOutcome<boolean>>;
  preparePagehideHandoff(
    owner: OutboundOwnerContext,
    expectedSmVersion: number | null,
    handoffToken: string,
    state: PersistedSmResumeState | null,
  ): Promise<DurableOutcome<DurableSmMutationResult<PagehideHandoffResult>>>;
  cancelOwnerHandoff(
    owner: OutboundOwnerContext,
    expectedToken: string,
    expectedSmVersion: number,
  ): Promise<DurableOutcome<PagehideHandoffCancelResult>>;
}

export function roomOutboundLane(roomJid: string): RoomOutboundLane {
  const canonicalRoomJid = bareJidKey(roomJid);
  if (!canonicalRoomJid) {
    throw new DOMException("Outbound room lane requires a room JID", "DataError");
  }
  return { kind: "room", roomJid: canonicalRoomJid };
}

export function outboundLane(message: PersistedQueuedMessage): OutboundLane {
  if (message.kind === "dm") return { kind: "direct" };
  return roomOutboundLane(message.roomJid);
}

export function createOutboundClaim(
  owner: OutboundOwnerContext,
  connectionGeneration: number,
  phase: OutboundClaimPhase,
): OutboundClaimRequest {
  return {
    accountKey: owner.accountKey,
    ownerId: owner.ownerId,
    ownerInstanceId: owner.ownerInstanceId,
    ownerGeneration: owner.ownerGeneration,
    authorityEpoch: owner.authorityEpoch,
    connectionGeneration,
    claimId: crypto.randomUUID(),
    phase,
  };
}

export type DurableAuthorityClock = {
  now(): number;
};
