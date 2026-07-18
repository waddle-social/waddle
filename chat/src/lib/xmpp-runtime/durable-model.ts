import type { PersistedQueuedMessage } from "../outbound-queue-store";
import type { PersistedSmResumeState } from "../xmpp/sm-resume-types";
import { cloneSmResumeState } from "../xmpp/sm-resume-types";
import {
  DurablePredecessorCapacityError,
  OUTBOUND_CLAIM_LEASE_MS,
  RETAINED_PREDECESSOR_LIMIT,
  SM_SNAPSHOT_RETENTION_MS,
  type DurableOutboundEntry,
  type DurableOutboundEntryState,
  type DurableSmEnvelope,
  type OutboundClaim,
  type OutboundClaimRequest,
  type OutboundLane,
  type OutboundOwnerContext,
  type OutboundOwnerHandoff,
  type OutboundRowIdentity,
  type OutboundTerminalIntent,
} from "./durable-contract";

const DEFAULT_SM_RESUME_WINDOW_MS = 300_000;
export const OUTBOUND_OWNER_RETENTION_MS = 8 * 24 * 60 * 60 * 1_000;
export const AUTHORITY_CLOCK_ROLLBACK_TOLERANCE_MS = 1_000;

export function checkedDurableCounterIncrement(
  value: number,
  label: string,
): number {
  if (
    !Number.isSafeInteger(value)
    || value < 0
    || value >= Number.MAX_SAFE_INTEGER
  ) {
    throw new DOMException(`${label} counter exhausted`, "AbortError");
  }
  return value + 1;
}

export function checkedDurableDeadline(
  start: number,
  durationMs: number,
  label: string,
): number {
  if (
    !Number.isSafeInteger(start)
    || start < 0
    || !Number.isSafeInteger(durationMs)
    || durationMs < 0
    || start > Number.MAX_SAFE_INTEGER - durationMs
  ) {
    throw new DOMException(`${label} deadline exhausted`, "AbortError");
  }
  return start + durationMs;
}

export type DurableOutboundRow = {
  identity: OutboundRowIdentity;
  lane: OutboundLane;
  orderKey: string;
  message: PersistedQueuedMessage;
  state: DurableOutboundEntryState;
};

export type DurableOutboundOwner = {
  ownerId: string;
  ownerInstanceId: string;
  ownerGeneration: number;
  authorityEpoch: number;
  leaseUntil: number;
  lastRenewedAt: number;
  handoff?: OutboundOwnerHandoff;
  predecessors?: Array<{
    ownerInstanceId: string;
    ownerGeneration: number;
    authorityEpoch: number;
    expiresAt: number;
  }>;
};

export type DurablePredecessorFence =
  NonNullable<DurableOutboundOwner["predecessors"]>[number];

export type DurableSmRecord = {
  accountKey: string;
  ownerId: string;
  ownerGeneration: number;
  authorityEpoch: number;
  version: number;
  state: PersistedSmResumeState | null;
  savedAt: number;
  consumed: boolean;
};

export type RuntimeAccount = {
  accountKey: string;
  schemaVersion: 1;
  revision: number;
  lastAuthorityTimeMs: number;
  lastWallClockSampleMs: number;
  authorityEpoch: number;
  nextOwnerGeneration: number;
  owners: Record<string, DurableOutboundOwner>;
  outbound: Record<string, DurableOutboundRow>;
  terminals: Record<string, OutboundTerminalIntent>;
  smSnapshots: Record<string, DurableSmRecord>;
};

export type AccountMutation<T> = {
  changed: boolean;
  value: T;
  finalize?: (committedRevision: number) => T;
};

export function dictionary<T>(): Record<string, T> {
  return Object.create(null) as Record<string, T>;
}

export function emptyAccount(accountKey: string): RuntimeAccount {
  return {
    accountKey,
    schemaVersion: 1,
    revision: 0,
    lastAuthorityTimeMs: 0,
    lastWallClockSampleMs: 0,
    authorityEpoch: 0,
    nextOwnerGeneration: 1,
    owners: dictionary(),
    outbound: dictionary(),
    terminals: dictionary(),
    smSnapshots: dictionary(),
  };
}

export function cloneValue<T>(value: T): T {
  return structuredClone(value);
}

export function cloneDurableSmState(
  state: PersistedSmResumeState,
): PersistedSmResumeState {
  return cloneSmResumeState(state);
}

export function entryFromRow(row: DurableOutboundRow): DurableOutboundEntry {
  return {
    identity: { ...row.identity },
    lane: { ...row.lane },
    message: cloneValue(row.message),
    state: cloneValue(row.state),
  };
}

export function smEnvelope(record: DurableSmRecord): DurableSmEnvelope | null {
  if (!record.state) return null;
  return {
    accountKey: record.accountKey,
    ownerId: record.ownerId,
    ownerGeneration: record.ownerGeneration,
    authorityEpoch: record.authorityEpoch,
    version: record.version,
    state: cloneDurableSmState(record.state),
    savedAt: record.savedAt,
    consumed: record.consumed,
  };
}

export function sameSmFence(
  record: DurableSmRecord | undefined,
  owner: OutboundOwnerContext,
): record is DurableSmRecord {
  return !!record
    && record.accountKey === owner.accountKey
    && record.ownerId === owner.ownerId
    && record.ownerGeneration === owner.ownerGeneration
    && record.authorityEpoch === owner.authorityEpoch;
}

export function ownerFence(
  accountKey: string,
  owner: DurableOutboundOwner,
): OutboundOwnerContext {
  return {
    accountKey,
    ownerId: owner.ownerId,
    ownerInstanceId: owner.ownerInstanceId,
    ownerGeneration: owner.ownerGeneration,
    authorityEpoch: owner.authorityEpoch,
  };
}

export function sameOwner(
  persisted: DurableOutboundOwner | undefined,
  expected: OutboundOwnerContext,
): boolean {
  return !!persisted
    && persisted.ownerId === expected.ownerId
    && persisted.ownerInstanceId === expected.ownerInstanceId
    && persisted.ownerGeneration === expected.ownerGeneration
    && persisted.authorityEpoch === expected.authorityEpoch;
}

export function currentOwner(
  account: RuntimeAccount,
  expected: OutboundOwnerContext,
  now: number,
): DurableOutboundOwner | null {
  if (expected.accountKey !== account.accountKey) return null;
  if (expected.authorityEpoch !== account.authorityEpoch) return null;
  const persisted = account.owners[expected.ownerId];
  return sameOwner(persisted, expected) && persisted.leaseUntil > now
    ? persisted
    : null;
}

export function allocateOwnerGeneration(account: RuntimeAccount): number {
  const generation = account.nextOwnerGeneration;
  if (!Number.isSafeInteger(generation) || generation <= 0) {
    throw new DOMException("Outbound owner generation exhausted", "AbortError");
  }
  account.nextOwnerGeneration = checkedDurableCounterIncrement(
    generation,
    "Outbound owner generation",
  );
  return generation;
}

export function sameIdentity(
  left: OutboundRowIdentity,
  right: OutboundRowIdentity,
): boolean {
  return left.accountKey === right.accountKey
    && left.messageId === right.messageId
    && left.incarnation === right.incarnation
    && left.payloadDigest === right.payloadDigest;
}

export function sameClaim(
  left: OutboundClaim | undefined,
  right: OutboundClaim,
): boolean {
  return !!left
    && left.accountKey === right.accountKey
    && left.ownerId === right.ownerId
    && left.ownerInstanceId === right.ownerInstanceId
    && left.ownerGeneration === right.ownerGeneration
    && left.authorityEpoch === right.authorityEpoch
    && left.connectionGeneration === right.connectionGeneration
    && left.claimId === right.claimId
    && left.phase === right.phase
    && left.rowIncarnation === right.rowIncarnation
    && left.payloadDigest === right.payloadDigest;
}

export function claimMatchesIdentity(
  claim: Pick<OutboundClaim, "accountKey" | "rowIncarnation" | "payloadDigest">,
  identity: OutboundRowIdentity,
): boolean {
  return claim.accountKey === identity.accountKey
    && claim.rowIncarnation === identity.incarnation
    && claim.payloadDigest === identity.payloadDigest;
}

export function claimForRow(
  request: OutboundClaimRequest,
  identity: OutboundRowIdentity,
  authorityNow: number,
): OutboundClaim {
  return {
    ...request,
    rowIncarnation: identity.incarnation,
    payloadDigest: identity.payloadDigest,
    leaseUntil: checkedDurableDeadline(
      authorityNow,
      OUTBOUND_CLAIM_LEASE_MS,
      "Outbound claim lease",
    ),
  };
}

export function orderKey(message: PersistedQueuedMessage): string {
  return `${message.createdAt}\u0000${message.id}`;
}

export function sameLane(left: OutboundLane, right: OutboundLane): boolean {
  return left.kind === right.kind
    && (left.kind === "direct"
      || (right.kind === "room" && left.roomJid === right.roomJid));
}

export function orderedRows(account: RuntimeAccount): DurableOutboundRow[] {
  return Object.values(account.outbound).sort((left, right) => (
    left.orderKey.localeCompare(right.orderKey)
  ));
}

function claimReferencesOwner(
  claim: OutboundClaim,
  owner: DurableOutboundOwner,
): boolean {
  return claim.ownerId === owner.ownerId
    && claim.ownerInstanceId === owner.ownerInstanceId
    && claim.ownerGeneration === owner.ownerGeneration
    && claim.authorityEpoch === owner.authorityEpoch;
}

export function claimReferencesPredecessor(
  claim: OutboundClaim,
  owner: DurableOutboundOwner,
): boolean {
  return (owner.predecessors ?? []).some((predecessor) => (
    claim.ownerId === owner.ownerId
    && claim.ownerInstanceId === predecessor.ownerInstanceId
    && claim.ownerGeneration === predecessor.ownerGeneration
    && claim.authorityEpoch === predecessor.authorityEpoch
  ));
}

function predecessorHasDurableReference(
  account: RuntimeAccount,
  owner: DurableOutboundOwner,
  predecessor: DurablePredecessorFence,
): boolean {
  const matches = (claim: OutboundClaim) => (
    claim.ownerId === owner.ownerId
    && claim.ownerInstanceId === predecessor.ownerInstanceId
    && claim.ownerGeneration === predecessor.ownerGeneration
    && claim.authorityEpoch === predecessor.authorityEpoch
  );
  return Object.values(account.outbound).some((row) => (
    row.state.kind === "claimed"
    && matches(row.state.claim)
  )) || Object.values(account.terminals).some((intent) => (
    matches(intent.expected)
  ));
}

export function pruneUnreferencedPredecessors(
  account: RuntimeAccount,
  owner: DurableOutboundOwner,
): boolean {
  const existing = owner.predecessors ?? [];
  const retained = existing.filter((predecessor) => (
    predecessorHasDurableReference(account, owner, predecessor)
  ));
  if (retained.length === existing.length) return false;
  if (retained.length === 0) {
    delete owner.predecessors;
  } else {
    owner.predecessors = retained;
  }
  return true;
}

export function retainedPredecessorsForHandoff(
  account: RuntimeAccount,
  owner: DurableOutboundOwner,
): DurablePredecessorFence[] {
  const retained = (owner.predecessors ?? []).filter((predecessor) => (
    predecessorHasDurableReference(account, owner, predecessor)
  ));
  const currentIsReferenced = Object.values(account.outbound).some((row) => (
    row.state.kind === "claimed"
    && claimReferencesOwner(row.state.claim, owner)
  )) || Object.values(account.terminals).some((intent) => (
    claimReferencesOwner(intent.expected, owner)
  ));
  if (currentIsReferenced) {
    retained.push({
      ownerInstanceId: owner.ownerInstanceId,
      ownerGeneration: owner.ownerGeneration,
      authorityEpoch: owner.authorityEpoch,
      expiresAt: owner.handoff?.expiresAt ?? owner.leaseUntil,
    });
  }
  if (retained.length > RETAINED_PREDECESSOR_LIMIT) {
    throw new DurablePredecessorCapacityError();
  }
  return retained;
}

export function smRecordRetentionDeadline(record: DurableSmRecord): number {
  if (!record.state || record.consumed) {
    return checkedDurableDeadline(
      record.savedAt,
      SM_SNAPSHOT_RETENTION_MS,
      "Durable SM record retention",
    );
  }
  const configuredSeconds = record.state.maxResumeSeconds;
  const resumeWindowMs = typeof configuredSeconds === "number"
    && Number.isInteger(configuredSeconds)
    && configuredSeconds > 0
    ? configuredSeconds * 1_000
    : DEFAULT_SM_RESUME_WINDOW_MS;
  const resumeDeadline = checkedDurableDeadline(
    record.savedAt,
    resumeWindowMs,
    "Durable SM resume window",
  );
  return checkedDurableDeadline(
    resumeDeadline,
    SM_SNAPSHOT_RETENTION_MS,
    "Durable SM record retention",
  );
}

export function ownerHasDurableReference(
  account: RuntimeAccount,
  owner: DurableOutboundOwner,
  authorityNow: number,
): boolean {
  if (
    owner.handoff
    && owner.handoff.expiresAt > authorityNow
    && owner.handoff.ownerGeneration === owner.ownerGeneration
    && owner.handoff.authorityEpoch === owner.authorityEpoch
  ) return true;
  if (Object.values(account.outbound).some((row) => (
    row.state.kind === "claimed"
    && (
      claimReferencesOwner(row.state.claim, owner)
      || claimReferencesPredecessor(row.state.claim, owner)
    )
  ))) return true;
  if (Object.values(account.terminals).some((intent) => (
    claimReferencesOwner(intent.expected, owner)
    || claimReferencesPredecessor(intent.expected, owner)
  ))) return true;
  const snapshot = account.smSnapshots[owner.ownerId];
  return !!snapshot
    && snapshot.ownerGeneration === owner.ownerGeneration
    && snapshot.authorityEpoch === owner.authorityEpoch;
}
