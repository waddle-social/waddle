import type { PersistedQueuedMessage } from "../outbound-queue-store";
import {
  OUTBOUND_CLAIM_LEASE_MS,
  type DurableOutboundScan,
  type OutboundClaim,
  type OutboundClaimHeadResult,
  type OutboundClaimPhase,
  type OutboundClaimRequest,
  type OutboundLane,
  type OutboundOwnerContext,
  type OutboundPersistClaimedResult,
  type OutboundPersistResult,
  type OutboundReleaseResult,
  type OutboundRenewResult,
  type OutboundRowIdentity,
  type OutboundTerminalApplyResult,
  type OutboundTerminalIntent,
  type OutboundTerminalKind,
  type OutboundTerminalRecordResult,
  type ResumeClaimReconciliation,
} from "./durable-contract";
import {
  OUTBOUND_OWNER_RETENTION_MS,
  checkedDurableDeadline,
  claimForRow,
  claimMatchesIdentity,
  claimReferencesPredecessor,
  cloneValue,
  currentOwner,
  entryFromRow,
  orderedRows,
  ownerHasDurableReference,
  pruneUnreferencedPredecessors,
  sameClaim,
  sameIdentity,
  sameLane,
  smRecordRetentionDeadline,
  type AccountMutation,
  type DurableOutboundRow,
  type RuntimeAccount,
} from "./durable-model";

export type PreparedOutboundMessage = {
  identity: OutboundRowIdentity;
  lane: OutboundLane;
  orderKey: string;
  message: PersistedQueuedMessage;
};

export type ReconcileResumeInput = {
  owner: OutboundOwnerContext;
  connectionGeneration: number;
  authoritativeMessageIds: readonly string[] | null;
  phase: Extract<OutboundClaimPhase, "resume-replay" | "fresh-fallback">;
  claimIds: ReadonlyMap<string, string>;
};

export function revisionTransition(
  account: RuntimeAccount,
): AccountMutation<number> {
  return {
    changed: false,
    value: account.revision,
    finalize: (committedRevision) => committedRevision,
  };
}

export function listOutboundTransition(
  account: RuntimeAccount,
): AccountMutation<PersistedQueuedMessage[]> {
  return {
    changed: false,
    value: orderedRows(account).map((row) => cloneValue(row.message)),
  };
}

export function scanAndPruneTransition(
  account: RuntimeAccount,
  authorityNow: number,
  cutoff: number,
): AccountMutation<DurableOutboundScan> {
  const pruned: OutboundRowIdentity[] = [];
  let metadataPruned = false;
  for (const [messageId, row] of Object.entries(account.outbound)) {
    const createdAt = Date.parse(row.message.createdAt);
    if (!Number.isFinite(createdAt) || createdAt >= cutoff) continue;
    if (row.state.kind === "terminal") continue;
    if (
      row.state.kind === "claimed"
      && row.state.claim.leaseUntil > authorityNow
    ) continue;
    pruned.push({ ...row.identity });
    delete account.outbound[messageId];
  }
  for (const [ownerId, record] of Object.entries(account.smSnapshots)) {
    const exactOwner = account.owners[ownerId];
    if (
      exactOwner
      && exactOwner.ownerGeneration === record.ownerGeneration
      && exactOwner.authorityEpoch === record.authorityEpoch
      && exactOwner.leaseUntil > authorityNow
    ) continue;
    if (authorityNow <= smRecordRetentionDeadline(record)) continue;
    delete account.smSnapshots[ownerId];
    metadataPruned = true;
  }
  for (const [ownerId, owner] of Object.entries(account.owners)) {
    if (pruneUnreferencedPredecessors(account, owner)) {
      metadataPruned = true;
    }
    if (owner.leaseUntil > authorityNow) continue;
    const lastActiveAt = owner.lastRenewedAt
      ?? Math.max(0, owner.leaseUntil - OUTBOUND_CLAIM_LEASE_MS);
    if (
      authorityNow <= checkedDurableDeadline(
        lastActiveAt,
        OUTBOUND_OWNER_RETENTION_MS,
        "Outbound owner retention",
      )
    ) continue;
    if (ownerHasDurableReference(account, owner, authorityNow)) continue;
    delete account.owners[ownerId];
    metadataPruned = true;
  }
  const value = {
    entries: orderedRows(account).map(entryFromRow),
    pruned,
    revision: account.revision,
  };
  return {
    changed: pruned.length > 0 || metadataPruned,
    value,
    finalize: (committedRevision) => ({
      ...value,
      revision: committedRevision,
    }),
  };
}

export function persistReadyTransition(
  account: RuntimeAccount,
  prepared: PreparedOutboundMessage,
): AccountMutation<OutboundPersistResult> {
  const existing = account.outbound[prepared.message.id];
  if (existing) {
    if (existing.identity.payloadDigest !== prepared.identity.payloadDigest) {
      return {
        changed: false,
        value: {
          kind: "conflict",
          messageId: prepared.message.id,
          existingPayloadDigest: existing.identity.payloadDigest,
          attemptedPayloadDigest: prepared.identity.payloadDigest,
        },
      };
    }
    return {
      changed: false,
      value: { kind: "existing", entry: entryFromRow(existing) },
    };
  }
  const row: DurableOutboundRow = {
    identity: prepared.identity,
    lane: prepared.lane,
    orderKey: prepared.orderKey,
    message: prepared.message,
    state: { kind: "ready" },
  };
  account.outbound[prepared.message.id] = row;
  return {
    changed: true,
    value: { kind: "inserted", entry: entryFromRow(row) },
  };
}

export function persistClaimedTransition(
  account: RuntimeAccount,
  authorityNow: number,
  prepared: PreparedOutboundMessage,
  request: OutboundClaimRequest,
): AccountMutation<OutboundPersistClaimedResult> {
  if (!currentOwner(account, request, authorityNow)) {
    return { changed: false, value: { kind: "fenced" } };
  }
  const existing = account.outbound[prepared.message.id];
  if (existing) {
    if (existing.identity.payloadDigest !== prepared.identity.payloadDigest) {
      return {
        changed: false,
        value: {
          kind: "conflict",
          messageId: prepared.message.id,
          existingPayloadDigest: existing.identity.payloadDigest,
          attemptedPayloadDigest: prepared.identity.payloadDigest,
        },
      };
    }
    if (existing.state.kind === "terminal") {
      return {
        changed: false,
        value: { kind: "terminal", entry: entryFromRow(existing) },
      };
    }
    if (
      existing.state.kind === "claimed"
      && existing.state.claim.leaseUntil > authorityNow
    ) {
      return {
        changed: false,
        value: {
          kind: "busy",
          entry: entryFromRow(existing),
          leaseUntil: existing.state.claim.leaseUntil,
        },
      };
    }
    const claim = claimForRow(request, existing.identity, authorityNow);
    existing.state = { kind: "claimed", claim };
    return {
      changed: true,
      value: {
        kind: "claimed",
        entry: entryFromRow(existing),
        claim: { ...claim },
      },
    };
  }
  const claim = claimForRow(request, prepared.identity, authorityNow);
  const row: DurableOutboundRow = {
    identity: prepared.identity,
    lane: prepared.lane,
    orderKey: prepared.orderKey,
    message: prepared.message,
    state: { kind: "claimed", claim },
  };
  account.outbound[prepared.message.id] = row;
  return {
    changed: true,
    value: {
      kind: "claimed",
      entry: entryFromRow(row),
      claim: { ...claim },
    },
  };
}

export function claimHeadTransition(
  account: RuntimeAccount,
  authorityNow: number,
  lane: OutboundLane,
  request: OutboundClaimRequest,
): AccountMutation<OutboundClaimHeadResult> {
  if (!currentOwner(account, request, authorityNow)) {
    return { changed: false, value: { kind: "fenced" } };
  }
  const head = orderedRows(account).find((row) => sameLane(row.lane, lane));
  if (!head) return { changed: false, value: { kind: "missing" } };
  if (head.state.kind === "terminal") {
    return {
      changed: false,
      value: { kind: "terminal", messageId: head.identity.messageId },
    };
  }
  if (
    head.state.kind === "claimed"
    && head.state.claim.leaseUntil > authorityNow
  ) {
    return {
      changed: false,
      value: {
        kind: "busy",
        messageId: head.identity.messageId,
        leaseUntil: head.state.claim.leaseUntil,
      },
    };
  }
  const claim = claimForRow(request, head.identity, authorityNow);
  head.state = { kind: "claimed", claim };
  return {
    changed: true,
    value: {
      kind: "claimed",
      entry: entryFromRow(head),
      claim: { ...claim },
    },
  };
}

export function renewClaimTransition(
  account: RuntimeAccount,
  authorityNow: number,
  identity: OutboundRowIdentity,
  expected: OutboundClaim,
): AccountMutation<OutboundRenewResult> {
  if (!currentOwner(account, expected, authorityNow)) {
    return { changed: false, value: { kind: "fenced" } };
  }
  const row = account.outbound[identity.messageId];
  if (
    !row
    || !sameIdentity(row.identity, identity)
    || row.state.kind !== "claimed"
    || !sameClaim(row.state.claim, expected)
  ) {
    return { changed: false, value: { kind: "missing" } };
  }
  if (row.state.claim.leaseUntil <= authorityNow) {
    return { changed: false, value: { kind: "missing" } };
  }
  const claim = {
    ...row.state.claim,
    leaseUntil: checkedDurableDeadline(
      authorityNow,
      OUTBOUND_CLAIM_LEASE_MS,
      "Outbound claim lease",
    ),
  };
  row.state = { kind: "claimed", claim };
  return {
    changed: true,
    value: { kind: "renewed", claim: { ...claim } },
  };
}

export function releaseClaimTransition(
  account: RuntimeAccount,
  authorityNow: number,
  identity: OutboundRowIdentity,
  expected: OutboundClaim,
): AccountMutation<OutboundReleaseResult> {
  if (!currentOwner(account, expected, authorityNow)) {
    return { changed: false, value: { kind: "fenced" } };
  }
  const row = account.outbound[identity.messageId];
  if (
    !row
    || !sameIdentity(row.identity, identity)
    || row.state.kind !== "claimed"
    || !sameClaim(row.state.claim, expected)
  ) {
    return { changed: false, value: { kind: "missing" } };
  }
  row.state = { kind: "ready" };
  return { changed: true, value: { kind: "released" } };
}

export function reconcileResumeClaimsTransition(
  account: RuntimeAccount,
  authorityNow: number,
  input: ReconcileResumeInput,
): AccountMutation<ResumeClaimReconciliation> {
  const {
    owner,
    connectionGeneration,
    authoritativeMessageIds,
    phase,
    claimIds,
  } = input;
  const authoritative = authoritativeMessageIds === null
    ? null
    : new Set(authoritativeMessageIds);
  const persistedOwner = currentOwner(account, owner, authorityNow);
  if (!persistedOwner) {
    return { changed: false, value: { kind: "fenced" } };
  }

  const claims: Array<{ messageId: string; claim: OutboundClaim }> = [];
  const plannedClaims: Array<{
    messageId: string;
    claim: OutboundClaim;
  }> = [];
  const plannedReleases: string[] = [];
  const blockedIds: string[] = [];
  const terminalIds: string[] = [];
  const seenIds = new Set<string>();
  for (const row of orderedRows(account)) {
    const messageId = row.identity.messageId;
    const isAuthoritative = authoritative?.has(messageId) ?? false;
    if (isAuthoritative) seenIds.add(messageId);
    if (row.state.kind === "terminal") {
      if (isAuthoritative) terminalIds.push(messageId);
      continue;
    }
    const existing = row.state.kind === "claimed" ? row.state.claim : null;
    const exactOwner = !!existing
      && existing.ownerId === owner.ownerId
      && existing.ownerInstanceId === owner.ownerInstanceId
      && existing.ownerGeneration === owner.ownerGeneration
      && existing.authorityEpoch === owner.authorityEpoch;
    const predecessorOwned = !!existing
      && claimReferencesPredecessor(existing, persistedOwner);
    const expired = !!existing && existing.leaseUntil <= authorityNow;

    if (isAuthoritative) {
      if (existing && !exactOwner && !predecessorOwned && !expired) {
        blockedIds.push(messageId);
        continue;
      }
      if (
        existing
        && exactOwner
        && existing.connectionGeneration === connectionGeneration
        && existing.phase === phase
      ) {
        claims.push({ messageId, claim: { ...existing } });
        continue;
      }
      const claimId = claimIds.get(messageId);
      if (!claimId) {
        throw new DOMException(
          "Prepared resume claim identifier is missing",
          "AbortError",
        );
      }
      const claim = claimForRow({
        ...owner,
        connectionGeneration,
        claimId,
        phase,
      }, row.identity, authorityNow);
      claims.push({ messageId, claim: { ...claim } });
      plannedClaims.push({ messageId, claim });
      continue;
    }

    if (!existing || (!exactOwner && !predecessorOwned)) continue;
    const preserveCurrentFallback = authoritative !== null
      && exactOwner
      && existing.connectionGeneration === connectionGeneration
      && existing.phase === "fresh-fallback";
    if (preserveCurrentFallback) continue;
    plannedReleases.push(messageId);
  }
  const missingIds = authoritative === null
    ? []
    : [...authoritative].filter((messageId) => !seenIds.has(messageId));

  if (blockedIds.length > 0 || terminalIds.length > 0 || missingIds.length > 0) {
    return {
      changed: false,
      value: {
        kind: "reconciled",
        claims: [],
        releasedIds: [],
        blockedIds,
        terminalIds,
        missingIds,
      },
    };
  }

  for (const { messageId, claim } of plannedClaims) {
    const row = account.outbound[messageId];
    if (!row) {
      throw new DOMException(
        "Validated outbound row disappeared inside one transaction",
        "AbortError",
      );
    }
    row.state = { kind: "claimed", claim };
  }
  for (const messageId of plannedReleases) {
    const row = account.outbound[messageId];
    if (!row) {
      throw new DOMException(
        "Validated outbound row disappeared inside one transaction",
        "AbortError",
      );
    }
    row.state = { kind: "ready" };
  }
  let changed = plannedClaims.length > 0 || plannedReleases.length > 0;
  if (pruneUnreferencedPredecessors(account, persistedOwner)) {
    changed = true;
  }
  return {
    changed,
    value: {
      kind: "reconciled",
      claims,
      releasedIds: plannedReleases,
      blockedIds,
      terminalIds,
      missingIds,
    },
  };
}

export function releaseForFreshSessionTransition(
  account: RuntimeAccount,
  authorityNow: number,
  owner: OutboundOwnerContext,
  connectionGeneration: number,
): AccountMutation<string[] | null> {
  if (!currentOwner(account, owner, authorityNow)) {
    return { changed: false, value: null };
  }
  const released: string[] = [];
  for (const row of orderedRows(account)) {
    if (row.state.kind !== "claimed") continue;
    const claim = row.state.claim;
    if (
      claim.ownerId !== owner.ownerId
      || claim.ownerInstanceId !== owner.ownerInstanceId
      || claim.ownerGeneration !== owner.ownerGeneration
      || claim.authorityEpoch !== owner.authorityEpoch
    ) continue;
    if (
      claim.connectionGeneration === connectionGeneration
      && claim.phase === "fresh-fallback"
    ) continue;
    row.state = { kind: "ready" };
    released.push(row.identity.messageId);
  }
  return { changed: released.length > 0, value: released };
}

export function listTerminalTransition(
  account: RuntimeAccount,
): AccountMutation<OutboundTerminalIntent[]> {
  return {
    changed: false,
    value: Object.values(account.terminals)
      .sort((left, right) => left.recordedAt - right.recordedAt)
      .map(cloneValue),
  };
}

export function recordTerminalTransition(
  account: RuntimeAccount,
  authorityNow: number,
  identity: OutboundRowIdentity,
  kind: OutboundTerminalKind,
  expected: OutboundClaim,
  intentId: string,
): AccountMutation<OutboundTerminalRecordResult> {
  const row = account.outbound[identity.messageId];
  if (!row || !sameIdentity(row.identity, identity)) {
    return { changed: false, value: { kind: "missing" } };
  }
  if (row.state.kind === "terminal") {
    const existing = account.terminals[row.state.intentId];
    return existing
      ? {
          changed: false,
          value: { kind: "recorded", intent: cloneValue(existing) },
        }
      : { changed: false, value: { kind: "stale" } };
  }
  if (!currentOwner(account, expected, authorityNow)) {
    return { changed: false, value: { kind: "fenced" } };
  }
  if (row.state.kind !== "claimed" || !sameClaim(row.state.claim, expected)) {
    return { changed: false, value: { kind: "stale" } };
  }
  const intent: OutboundTerminalIntent = {
    intentId,
    accountKey: identity.accountKey,
    identity: { ...identity },
    kind,
    expected: { ...expected },
    recordedAt: authorityNow,
  };
  row.state = { kind: "terminal", intentId };
  account.terminals[intentId] = intent;
  return {
    changed: true,
    value: { kind: "recorded", intent: cloneValue(intent) },
  };
}

export function applyTerminalTransition(
  account: RuntimeAccount,
  authorityNow: number,
  executor: OutboundOwnerContext,
  intent: OutboundTerminalIntent,
): AccountMutation<OutboundTerminalApplyResult> {
  if (
    executor.accountKey !== intent.accountKey
    || !currentOwner(account, executor, authorityNow)
  ) {
    return { changed: false, value: { kind: "fenced" } };
  }
  const persisted = account.terminals[intent.intentId];
  if (
    !claimMatchesIdentity(intent.expected, intent.identity)
    || !persisted
    || !sameIdentity(persisted.identity, intent.identity)
    || !sameClaim(persisted.expected, intent.expected)
    || persisted.kind !== intent.kind
    || !claimMatchesIdentity(persisted.expected, persisted.identity)
  ) {
    return { changed: false, value: { kind: "missing" } };
  }
  const row = account.outbound[intent.identity.messageId];
  if (
    !row
    || !sameIdentity(row.identity, intent.identity)
    || !claimMatchesIdentity(persisted.expected, row.identity)
  ) {
    delete account.terminals[intent.intentId];
    return { changed: true, value: { kind: "missing" } };
  }
  if (row.state.kind !== "terminal" || row.state.intentId !== intent.intentId) {
    delete account.terminals[intent.intentId];
    return { changed: true, value: { kind: "stale" } };
  }

  if (persisted.kind === "ack" || persisted.kind === "nonretryable-delete") {
    delete account.outbound[intent.identity.messageId];
    delete account.terminals[intent.intentId];
    return {
      changed: true,
      value: persisted.kind === "ack"
        ? { kind: "acked", identity: { ...row.identity } }
        : { kind: "removed", identity: { ...row.identity } },
    };
  }

  if (
    persisted.expected.phase === "resume-replay"
    && currentOwner(account, persisted.expected, authorityNow)
  ) {
    const claim: OutboundClaim = {
      ...persisted.expected,
      phase: "fresh-fallback",
      leaseUntil: checkedDurableDeadline(
        authorityNow,
        OUTBOUND_CLAIM_LEASE_MS,
        "Outbound claim lease",
      ),
    };
    row.state = { kind: "claimed", claim };
    delete account.terminals[intent.intentId];
    return {
      changed: true,
      value: {
        kind: "fallback",
        identity: { ...row.identity },
        claim: { ...claim },
      },
    };
  }

  row.state = { kind: "ready" };
  delete account.terminals[intent.intentId];
  return {
    changed: true,
    value: { kind: "released", identity: { ...row.identity } },
  };
}
