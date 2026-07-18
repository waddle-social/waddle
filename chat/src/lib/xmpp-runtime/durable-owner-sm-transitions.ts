import type { PersistedSmResumeState } from "../xmpp/sm-resume-types";
import {
  OUTBOUND_CLAIM_LEASE_MS,
  type DurableSmClearResult,
  type DurableSmEnvelope,
  type DurableSmLoadResult,
  type DurableSmMutationResult,
  type OutboundOwnerActivation,
  type OutboundOwnerContext,
  type OutboundOwnerHint,
  type PagehideHandoffCancelResult,
  type PagehideHandoffResult,
} from "./durable-contract";
import {
  allocateOwnerGeneration,
  checkedDurableCounterIncrement,
  checkedDurableDeadline,
  cloneDurableSmState,
  cloneValue,
  currentOwner,
  ownerFence,
  retainedPredecessorsForHandoff,
  sameSmFence,
  smEnvelope,
  type AccountMutation,
  type DurableOutboundOwner,
  type RuntimeAccount,
} from "./durable-model";

export function claimOwnerTransition(
  account: RuntimeAccount,
  authorityNow: number,
  hint: OutboundOwnerHint,
  rotatedOwnerId: string,
): AccountMutation<OutboundOwnerActivation> {
  const existing = account.owners[hint.ownerId];
  let owner: DurableOutboundOwner;
  let handoffSm: DurableSmEnvelope | undefined;
  if (!existing) {
    owner = {
      ownerId: hint.ownerId,
      ownerInstanceId: hint.ownerInstanceId,
      ownerGeneration: allocateOwnerGeneration(account),
      authorityEpoch: account.authorityEpoch,
      leaseUntil: checkedDurableDeadline(
        authorityNow,
        OUTBOUND_CLAIM_LEASE_MS,
        "Outbound owner lease",
      ),
      lastRenewedAt: authorityNow,
    };
  } else if (
    existing.leaseUntil > authorityNow
    && existing.ownerInstanceId === hint.ownerInstanceId
    && existing.authorityEpoch === account.authorityEpoch
  ) {
    owner = {
      ...existing,
      leaseUntil: checkedDurableDeadline(
        authorityNow,
        OUTBOUND_CLAIM_LEASE_MS,
        "Outbound owner lease",
      ),
      lastRenewedAt: authorityNow,
    };
  } else if (
    existing.leaseUntil > authorityNow
    && existing.authorityEpoch === account.authorityEpoch
    && hint.handoffToken
    && existing.handoff?.token === hint.handoffToken
    && existing.handoff.expiresAt > authorityNow
    && existing.handoff.authorityEpoch === existing.authorityEpoch
    && existing.handoff.ownerGeneration === existing.ownerGeneration
  ) {
    const predecessors = retainedPredecessorsForHandoff(account, existing);
    owner = {
      ownerId: existing.ownerId,
      ownerInstanceId: hint.ownerInstanceId,
      ownerGeneration: allocateOwnerGeneration(account),
      authorityEpoch: account.authorityEpoch,
      leaseUntil: checkedDurableDeadline(
        authorityNow,
        OUTBOUND_CLAIM_LEASE_MS,
        "Outbound owner lease",
      ),
      lastRenewedAt: authorityNow,
      ...(predecessors.length > 0 ? { predecessors } : {}),
    };
    const smRecord = account.smSnapshots[existing.ownerId];
    const envelope = smRecord ? smEnvelope(smRecord) : null;
    if (
      smRecord
      && smRecord.ownerGeneration === existing.ownerGeneration
      && smRecord.authorityEpoch === existing.authorityEpoch
    ) {
      const handoffVersion = checkedDurableCounterIncrement(
        smRecord.version,
        "Durable SM version",
      );
      account.smSnapshots[existing.ownerId] = {
        ...smRecord,
        ownerGeneration: owner.ownerGeneration,
        authorityEpoch: owner.authorityEpoch,
        version: handoffVersion,
        consumed: true,
      };
      if (envelope && !smRecord.consumed) {
        handoffSm = {
          ...envelope,
          ownerGeneration: owner.ownerGeneration,
          authorityEpoch: owner.authorityEpoch,
          version: handoffVersion,
          consumed: true,
        };
      }
    }
  } else if (existing.ownerInstanceId === hint.ownerInstanceId) {
    owner = {
      ownerId: rotatedOwnerId,
      ownerInstanceId: hint.ownerInstanceId,
      ownerGeneration: allocateOwnerGeneration(account),
      authorityEpoch: account.authorityEpoch,
      leaseUntil: checkedDurableDeadline(
        authorityNow,
        OUTBOUND_CLAIM_LEASE_MS,
        "Outbound owner lease",
      ),
      lastRenewedAt: authorityNow,
    };
  } else {
    owner = {
      ownerId: rotatedOwnerId,
      ownerInstanceId: hint.ownerInstanceId,
      ownerGeneration: allocateOwnerGeneration(account),
      authorityEpoch: account.authorityEpoch,
      leaseUntil: checkedDurableDeadline(
        authorityNow,
        OUTBOUND_CLAIM_LEASE_MS,
        "Outbound owner lease",
      ),
      lastRenewedAt: authorityNow,
    };
  }
  account.owners[owner.ownerId] = owner;
  return {
    changed: true,
    value: {
      fence: ownerFence(account.accountKey, owner),
      ...(handoffSm ? { handoffSm: cloneValue(handoffSm) } : {}),
    },
  };
}

export function renewOwnerTransition(
  account: RuntimeAccount,
  authorityNow: number,
  owner: OutboundOwnerContext,
): AccountMutation<boolean> {
  const persisted = account.owners[owner.ownerId];
  if (!currentOwner(account, owner, authorityNow)) {
    return { changed: false, value: false };
  }
  persisted.leaseUntil = checkedDurableDeadline(
    authorityNow,
    OUTBOUND_CLAIM_LEASE_MS,
    "Outbound owner lease",
  );
  persisted.lastRenewedAt = authorityNow;
  return { changed: true, value: true };
}

export function preparePagehideHandoffTransition(
  account: RuntimeAccount,
  authorityNow: number,
  owner: OutboundOwnerContext,
  expectedSmVersion: number | null,
  handoffToken: string,
  snapshot: PersistedSmResumeState | null,
): AccountMutation<DurableSmMutationResult<PagehideHandoffResult>> {
  const persisted = account.owners[owner.ownerId];
  if (!currentOwner(account, owner, authorityNow)) {
    return { changed: false, value: { kind: "fenced" } };
  }
  const existing = account.smSnapshots[owner.ownerId];
  const actualVersion = sameSmFence(existing, owner)
    ? existing.version
    : null;
  if (actualVersion !== expectedSmVersion) {
    return {
      changed: false,
      value: { kind: "stale", actualVersion },
    };
  }
  const version = checkedDurableCounterIncrement(
    existing?.version ?? 0,
    "Durable SM version",
  );
  account.smSnapshots[owner.ownerId] = {
    accountKey: owner.accountKey,
    ownerId: owner.ownerId,
    ownerGeneration: owner.ownerGeneration,
    authorityEpoch: owner.authorityEpoch,
    version,
    state: snapshot,
    savedAt: authorityNow,
    consumed: snapshot === null,
  };
  const handoffDeadline = checkedDurableDeadline(
    authorityNow,
    OUTBOUND_CLAIM_LEASE_MS,
    "Outbound handoff",
  );
  const handoff = {
    token: handoffToken,
    expiresAt: handoffDeadline,
    authorityEpoch: owner.authorityEpoch,
    ownerGeneration: owner.ownerGeneration,
  };
  persisted.handoff = handoff;
  persisted.leaseUntil = Math.max(persisted.leaseUntil, handoffDeadline);
  persisted.lastRenewedAt = authorityNow;
  return {
    changed: true,
    value: {
      kind: "applied",
      value: { handoff: { ...handoff }, smVersion: version },
    },
  };
}

export function cancelOwnerHandoffTransition(
  account: RuntimeAccount,
  authorityNow: number,
  owner: OutboundOwnerContext,
  expectedToken: string,
  expectedSmVersion: number,
): AccountMutation<PagehideHandoffCancelResult> {
  const persisted = account.owners[owner.ownerId];
  if (!currentOwner(account, owner, authorityNow)) {
    return { changed: false, value: { kind: "fenced" } };
  }
  const smRecord = account.smSnapshots[owner.ownerId];
  if (
    persisted.handoff?.token !== expectedToken
    || persisted.handoff.authorityEpoch !== owner.authorityEpoch
    || persisted.handoff.ownerGeneration !== owner.ownerGeneration
    || !sameSmFence(smRecord, owner)
    || smRecord.version !== expectedSmVersion
  ) {
    return {
      changed: false,
      value: {
        kind: "stale",
        actualToken: persisted.handoff?.token ?? null,
        actualSmVersion: sameSmFence(smRecord, owner)
          ? smRecord.version
          : null,
      },
    };
  }
  delete persisted.handoff;
  return {
    changed: true,
    value: { kind: "applied", cancelled: true },
  };
}

export function loadSmTransition(
  account: RuntimeAccount,
  authorityNow: number,
  owner: OutboundOwnerContext,
): AccountMutation<DurableSmLoadResult> {
  if (!currentOwner(account, owner, authorityNow)) {
    return { changed: false, value: { kind: "fenced" } };
  }
  const envelope = account.smSnapshots[owner.ownerId];
  const ownedEnvelope = sameSmFence(envelope, owner)
    ? envelope
    : undefined;
  return {
    changed: false,
    value: {
      kind: "loaded",
      envelope: ownedEnvelope ? smEnvelope(ownedEnvelope) : null,
      version: ownedEnvelope?.version ?? null,
    },
  };
}

export function consumeSmTransition(
  account: RuntimeAccount,
  authorityNow: number,
  owner: OutboundOwnerContext,
  expectedVersion: number | null,
  usable: (envelope: DurableSmEnvelope) => boolean,
): AccountMutation<DurableSmMutationResult<DurableSmEnvelope | null>> {
  if (!currentOwner(account, owner, authorityNow)) {
    return { changed: false, value: { kind: "fenced" } };
  }
  const record = account.smSnapshots[owner.ownerId];
  const ownedRecord = sameSmFence(record, owner) ? record : undefined;
  const actualVersion = ownedRecord?.version ?? null;
  if (actualVersion !== expectedVersion) {
    return {
      changed: false,
      value: { kind: "stale", actualVersion },
    };
  }
  const envelope = ownedRecord ? smEnvelope(ownedRecord) : null;
  if (
    !ownedRecord
    || !envelope
    || ownedRecord.consumed
    || !usable(cloneValue(envelope))
  ) {
    return {
      changed: false,
      value: { kind: "applied", value: null },
    };
  }
  const consumed: DurableSmEnvelope = {
    ...envelope,
    version: checkedDurableCounterIncrement(
      ownedRecord.version,
      "Durable SM version",
    ),
    consumed: true,
  };
  account.smSnapshots[owner.ownerId] = {
    ...ownedRecord,
    version: consumed.version,
    consumed: true,
  };
  return {
    changed: true,
    value: { kind: "applied", value: cloneValue(consumed) },
  };
}

export function saveSmTransition(
  account: RuntimeAccount,
  authorityNow: number,
  owner: OutboundOwnerContext,
  expectedVersion: number | null,
  snapshot: PersistedSmResumeState,
): AccountMutation<DurableSmMutationResult<DurableSmEnvelope>> {
  if (!currentOwner(account, owner, authorityNow)) {
    return { changed: false, value: { kind: "fenced" } };
  }
  const existing = account.smSnapshots[owner.ownerId];
  const ownedExisting = sameSmFence(existing, owner) ? existing : undefined;
  const actualVersion = ownedExisting?.version ?? null;
  if (actualVersion !== expectedVersion) {
    return {
      changed: false,
      value: { kind: "stale", actualVersion },
    };
  }
  const envelope: DurableSmEnvelope = {
    accountKey: owner.accountKey,
    ownerId: owner.ownerId,
    ownerGeneration: owner.ownerGeneration,
    authorityEpoch: owner.authorityEpoch,
    version: checkedDurableCounterIncrement(
      existing?.version ?? 0,
      "Durable SM version",
    ),
    state: snapshot,
    savedAt: authorityNow,
    consumed: false,
  };
  account.smSnapshots[owner.ownerId] = {
    ...envelope,
    state: cloneDurableSmState(envelope.state),
  };
  return {
    changed: true,
    value: { kind: "applied", value: cloneValue(envelope) },
  };
}

export function clearSmTransition(
  account: RuntimeAccount,
  authorityNow: number,
  owner: OutboundOwnerContext,
  expectedVersion: number | null,
): AccountMutation<DurableSmMutationResult<DurableSmClearResult>> {
  if (!currentOwner(account, owner, authorityNow)) {
    return { changed: false, value: { kind: "fenced" } };
  }
  const existing = account.smSnapshots[owner.ownerId];
  const ownedExisting = sameSmFence(existing, owner) ? existing : undefined;
  const actualVersion = ownedExisting?.version ?? null;
  if (actualVersion !== expectedVersion) {
    return {
      changed: false,
      value: { kind: "stale", actualVersion },
    };
  }
  const version = checkedDurableCounterIncrement(
    existing?.version ?? 0,
    "Durable SM version",
  );
  account.smSnapshots[owner.ownerId] = {
    accountKey: owner.accountKey,
    ownerId: owner.ownerId,
    ownerGeneration: owner.ownerGeneration,
    authorityEpoch: owner.authorityEpoch,
    version,
    state: null,
    savedAt: authorityNow,
    consumed: true,
  };
  return {
    changed: true,
    value: {
      kind: "applied",
      value: { cleared: !!ownedExisting?.state, version },
    },
  };
}
