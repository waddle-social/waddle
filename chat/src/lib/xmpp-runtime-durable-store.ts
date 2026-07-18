import type { PersistedQueuedMessage } from "./outbound-queue-store";
import type { PersistedSmResumeState } from "./xmpp/sm-resume-types";
import { decodePersistedSmResumeState } from "./xmpp/sm-resume-types";
import {
  DurablePredecessorCapacityError,
  OUTBOUND_CLAIM_LEASE_MS,
  outboundLane,
  type DurableAuthorityClock,
  type DurableFailureReason,
  type DurableOutcome,
  type DurableOutboundScan,
  type DurableOutboundStore,
  type DurableSmClearResult,
  type DurableSmEnvelope,
  type DurableSmLoadResult,
  type DurableSmMutationResult,
  type OutboundClaim,
  type OutboundClaimHeadResult,
  type OutboundClaimPhase,
  type OutboundClaimRequest,
  type OutboundLane,
  type OutboundOwnerActivation,
  type OutboundOwnerContext,
  type OutboundOwnerHint,
  type OutboundPersistClaimedResult,
  type OutboundPersistResult,
  type OutboundReleaseResult,
  type OutboundRenewResult,
  type OutboundRowIdentity,
  type OutboundTerminalApplyResult,
  type OutboundTerminalIntent,
  type OutboundTerminalKind,
  type OutboundTerminalRecordResult,
  type PagehideHandoffCancelResult,
  type PagehideHandoffResult,
  type ResumeClaimReconciliation,
} from "./xmpp-runtime/durable-contract";
import {
  AUTHORITY_CLOCK_ROLLBACK_TOLERANCE_MS,
  OUTBOUND_OWNER_RETENTION_MS,
  allocateOwnerGeneration,
  checkedDurableCounterIncrement,
  checkedDurableDeadline,
  claimForRow,
  claimMatchesIdentity,
  claimReferencesPredecessor,
  cloneDurableSmState,
  cloneValue,
  currentOwner,
  emptyAccount,
  entryFromRow,
  orderKey,
  orderedRows,
  ownerFence,
  ownerHasDurableReference,
  pruneUnreferencedPredecessors,
  retainedPredecessorsForHandoff,
  sameClaim,
  sameIdentity,
  sameLane,
  sameSmFence,
  smEnvelope,
  smRecordRetentionDeadline,
  type AccountMutation,
  type DurableOutboundOwner,
  type DurableOutboundRow,
  type RuntimeAccount,
} from "./xmpp-runtime/durable-model";
import {
  decodeRuntimeAccount,
  outboundPayloadDigest,
} from "./xmpp-runtime/durable-codec";

const DATABASE_NAME = "waddle-chat-xmpp-runtime-v1";
const DATABASE_VERSION = 1;
const ACCOUNT_STORE_NAME = "accounts";

function classifyFailure(cause: unknown): DurableFailureReason {
  if (cause instanceof DurablePredecessorCapacityError) return "capacity";
  const name = cause instanceof DOMException || cause instanceof Error ? cause.name : "";
  if (name === "QuotaExceededError") return "quota";
  if (name === "SecurityError") return "security";
  if (name === "AbortError" || name === "TransactionInactiveError") return "aborted";
  return "unavailable";
}

function failed<T>(cause: unknown): DurableOutcome<T> {
  return { kind: "failed", reason: classifyFailure(cause), cause };
}

const systemAuthorityClock: DurableAuthorityClock = {
  now: () => Date.now(),
};

abstract class RuntimeDurableStore implements DurableOutboundStore {
  protected constructor(
    private readonly authorityClock: DurableAuthorityClock = systemAuthorityClock,
  ) {}

  protected abstract transact<T>(
    accountKey: string,
    mutate: (account: RuntimeAccount, authorityNow: number) => AccountMutation<T>,
  ): Promise<DurableOutcome<T>>;

  protected sampleAuthorityTime(account: RuntimeAccount): {
    authorityNow: number;
    metadataChanged: boolean;
    authorityEpochChanged: boolean;
  } {
    const sampled = this.authorityClock.now();
    if (!Number.isSafeInteger(sampled) || sampled < 0) {
      throw new DOMException("Durable authority clock is invalid", "AbortError");
    }
    const wallClockNow = sampled;
    const previousWallClock = account.lastWallClockSampleMs;
    const rolledBack = previousWallClock > 0
      && previousWallClock > AUTHORITY_CLOCK_ROLLBACK_TOLERANCE_MS
      && wallClockNow
        < previousWallClock - AUTHORITY_CLOCK_ROLLBACK_TOLERANCE_MS;
    if (rolledBack) {
      account.authorityEpoch = checkedDurableCounterIncrement(
        account.authorityEpoch,
        "Durable authority epoch",
      );
    }
    account.lastWallClockSampleMs = wallClockNow;
    return {
      authorityNow: Math.max(wallClockNow, account.lastAuthorityTimeMs),
      metadataChanged: rolledBack || wallClockNow !== previousWallClock,
      authorityEpochChanged: rolledBack,
    };
  }

  revision(accountKey: string): Promise<DurableOutcome<number>> {
    return this.transact(accountKey, (account) => ({
      changed: false,
      value: account.revision,
      finalize: (committedRevision) => committedRevision,
    }));
  }

  list(accountKey: string): Promise<DurableOutcome<PersistedQueuedMessage[]>> {
    return this.transact(accountKey, (account) => ({
      changed: false,
      value: orderedRows(account).map((row) => cloneValue(row.message)),
    }));
  }

  scanAndPrune(
    accountKey: string,
    cutoff: number,
  ): Promise<DurableOutcome<DurableOutboundScan>> {
    return this.transact<DurableOutboundScan>(accountKey, (account, authorityNow) => {
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
    });
  }

  async persistReady(
    accountKey: string,
    message: PersistedQueuedMessage,
  ): Promise<DurableOutcome<OutboundPersistResult>> {
    let identity: OutboundRowIdentity;
    try {
      identity = {
        accountKey,
        messageId: message.id,
        incarnation: crypto.randomUUID(),
        payloadDigest: await outboundPayloadDigest(message),
      };
    } catch (error) {
      return failed(error);
    }
    return this.transact<OutboundPersistResult>(accountKey, (account) => {
      const existing = account.outbound[message.id];
      if (existing) {
        if (existing.identity.payloadDigest !== identity.payloadDigest) {
          return {
            changed: false,
            value: {
              kind: "conflict",
              messageId: message.id,
              existingPayloadDigest: existing.identity.payloadDigest,
              attemptedPayloadDigest: identity.payloadDigest,
            },
          };
        }
        return {
          changed: false,
          value: { kind: "existing", entry: entryFromRow(existing) },
        };
      }
      const row: DurableOutboundRow = {
        identity,
        lane: outboundLane(message),
        orderKey: orderKey(message),
        message: cloneValue(message),
        state: { kind: "ready" },
      };
      account.outbound[message.id] = row;
      return {
        changed: true,
        value: { kind: "inserted", entry: entryFromRow(row) },
      };
    });
  }

  async persistClaimed(
    accountKey: string,
    message: PersistedQueuedMessage,
    request: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundPersistClaimedResult>> {
    let identity: OutboundRowIdentity;
    try {
      identity = {
        accountKey,
        messageId: message.id,
        incarnation: crypto.randomUUID(),
        payloadDigest: await outboundPayloadDigest(message),
      };
    } catch (error) {
      return failed(error);
    }
    return this.transact<OutboundPersistClaimedResult>(
      accountKey,
      (account, authorityNow) => {
      if (!currentOwner(account, request, authorityNow)) {
        return { changed: false, value: { kind: "fenced" } };
      }
      const existing = account.outbound[message.id];
      if (existing) {
        if (existing.identity.payloadDigest !== identity.payloadDigest) {
          return {
            changed: false,
            value: {
              kind: "conflict",
              messageId: message.id,
              existingPayloadDigest: existing.identity.payloadDigest,
              attemptedPayloadDigest: identity.payloadDigest,
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
          value: { kind: "claimed", entry: entryFromRow(existing), claim: { ...claim } },
        };
      }
      const claim = claimForRow(request, identity, authorityNow);
      const row: DurableOutboundRow = {
        identity,
        lane: outboundLane(message),
        orderKey: orderKey(message),
        message: cloneValue(message),
        state: { kind: "claimed", claim },
      };
      account.outbound[message.id] = row;
      return {
        changed: true,
        value: { kind: "claimed", entry: entryFromRow(row), claim: { ...claim } },
      };
      },
    );
  }

  claimHead(
    accountKey: string,
    lane: OutboundLane,
    request: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundClaimHeadResult>> {
    return this.transact<OutboundClaimHeadResult>(
      accountKey,
      (account, authorityNow) => {
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
        value: { kind: "claimed", entry: entryFromRow(head), claim: { ...claim } },
      };
      },
    );
  }

  renew(
    identity: OutboundRowIdentity,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<OutboundRenewResult>> {
    return this.transact<OutboundRenewResult>(identity.accountKey, (account, authorityNow) => {
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
    });
  }

  release(
    identity: OutboundRowIdentity,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<OutboundReleaseResult>> {
    return this.transact<OutboundReleaseResult>(identity.accountKey, (account, authorityNow) => {
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
    });
  }

  async reconcileResumeClaims(
    owner: OutboundOwnerContext,
    connectionGeneration: number,
    authoritativeMessageIds: readonly string[] | null,
    phase: Extract<OutboundClaimPhase, "resume-replay" | "fresh-fallback">,
  ): Promise<DurableOutcome<ResumeClaimReconciliation>> {
    const authoritative = authoritativeMessageIds === null
      ? null
      : new Set(authoritativeMessageIds);
    const claimIds = new Map(
      (authoritativeMessageIds ?? []).map((messageId) => [messageId, crypto.randomUUID()]),
    );
    return this.transact<ResumeClaimReconciliation>(
      owner.accountKey,
      (account, authorityNow) => {
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
          const claim = claimForRow({
            ...owner,
            connectionGeneration,
            claimId: claimIds.get(messageId)!,
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
        // This transaction operates on an isolated account value (IDB's
        // structured clone, or the memory adapter's clone). Returning
        // unchanged discards any tentative adoptions/releases above, so an
        // unresolved native snapshot can never partially transfer authority.
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

      // Only after the complete snapshot has passed validation do we apply
      // the off-side plan to the transaction clone. A clock/epoch metadata
      // write can therefore never persist partial claim adoption/release.
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
      },
    );
  }

  releaseForFreshSession(
    owner: OutboundOwnerContext,
    connectionGeneration: number,
  ): Promise<DurableOutcome<string[] | null>> {
    return this.transact(owner.accountKey, (account, authorityNow) => {
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
    });
  }

  listTerminal(
    accountKey: string,
  ): Promise<DurableOutcome<OutboundTerminalIntent[]>> {
    return this.transact(accountKey, (account) => ({
      changed: false,
      value: Object.values(account.terminals)
        .sort((left, right) => left.recordedAt - right.recordedAt)
        .map(cloneValue),
    }));
  }

  recordTerminal(
    identity: OutboundRowIdentity,
    kind: OutboundTerminalKind,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<OutboundTerminalRecordResult>> {
    const intentId = crypto.randomUUID();
    return this.transact<OutboundTerminalRecordResult>(
      identity.accountKey,
      (account, authorityNow) => {
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
      },
    );
  }

  applyTerminal(
    executor: OutboundOwnerContext,
    intent: OutboundTerminalIntent,
  ): Promise<DurableOutcome<OutboundTerminalApplyResult>> {
    return this.transact<OutboundTerminalApplyResult>(
      intent.accountKey,
      (account, authorityNow) => {
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
          value: { kind: "fallback", identity: { ...row.identity }, claim: { ...claim } },
        };
      }

      row.state = { kind: "ready" };
      delete account.terminals[intent.intentId];
      return {
        changed: true,
        value: { kind: "released", identity: { ...row.identity } },
      };
      },
    );
  }

  claimOwner(
    accountKey: string,
    hint: OutboundOwnerHint,
  ): Promise<DurableOutcome<OutboundOwnerActivation>> {
    const rotatedOwnerId = crypto.randomUUID();
    return this.transact(accountKey, (account, authorityNow) => {
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
          // An expired or epoch-stale lifecycle is historical authority even
          // when the caller presents the same process incarnation. Keep its
          // owner row (and any claims/intents) immutable under the old key;
          // the reactivation receives a fresh owner key and generation.
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
          fence: ownerFence(accountKey, owner),
          ...(handoffSm ? { handoffSm: cloneValue(handoffSm) } : {}),
        },
      };
    });
  }

  renewOwner(
    owner: OutboundOwnerContext,
  ): Promise<DurableOutcome<boolean>> {
    return this.transact<boolean>(owner.accountKey, (account, authorityNow) => {
      const persisted = account.owners[owner.ownerId];
      if (
        !currentOwner(account, owner, authorityNow)
      ) {
        return { changed: false, value: false };
      }
      persisted.leaseUntil = checkedDurableDeadline(
        authorityNow,
        OUTBOUND_CLAIM_LEASE_MS,
        "Outbound owner lease",
      );
      persisted.lastRenewedAt = authorityNow;
      return { changed: true, value: true };
    });
  }

  preparePagehideHandoff(
    owner: OutboundOwnerContext,
    expectedSmVersion: number | null,
    handoffToken: string,
    state: PersistedSmResumeState | null,
  ): Promise<DurableOutcome<DurableSmMutationResult<PagehideHandoffResult>>> {
    let snapshot: PersistedSmResumeState | null;
    try {
      snapshot = state
        ? decodePersistedSmResumeState(state, "pagehide.state")
        : null;
    } catch (error) {
      return Promise.resolve(failed(error));
    }
    return this.transact<DurableSmMutationResult<PagehideHandoffResult>>(
      owner.accountKey,
      (account, authorityNow) => {
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
      },
    );
  }

  cancelOwnerHandoff(
    owner: OutboundOwnerContext,
    expectedToken: string,
    expectedSmVersion: number,
  ): Promise<DurableOutcome<PagehideHandoffCancelResult>> {
    return this.transact<PagehideHandoffCancelResult>(
      owner.accountKey,
      (account, authorityNow) => {
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
      },
    );
  }

  loadSm(
    owner: OutboundOwnerContext,
  ): Promise<DurableOutcome<DurableSmLoadResult>> {
    return this.transact<DurableSmLoadResult>(
      owner.accountKey,
      (account, authorityNow) => {
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
      },
    );
  }

  consumeSm(
    owner: OutboundOwnerContext,
    expectedVersion: number | null,
    usable: (envelope: DurableSmEnvelope) => boolean,
  ): Promise<DurableOutcome<DurableSmMutationResult<DurableSmEnvelope | null>>> {
    return this.transact<DurableSmMutationResult<DurableSmEnvelope | null>>(
      owner.accountKey,
      (account, authorityNow) => {
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
      },
    );
  }

  saveSm(
    owner: OutboundOwnerContext,
    expectedVersion: number | null,
    state: PersistedSmResumeState,
    savedAt: number,
  ): Promise<DurableOutcome<DurableSmMutationResult<DurableSmEnvelope>>> {
    let snapshot: PersistedSmResumeState;
    try {
      snapshot = decodePersistedSmResumeState(state, "save.state");
    } catch (error) {
      return Promise.resolve(failed(error));
    }
    return this.transact<DurableSmMutationResult<DurableSmEnvelope>>(
      owner.accountKey,
      (account, authorityNow) => {
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
      },
    );
  }

  clearSm(
    owner: OutboundOwnerContext,
    expectedVersion: number | null,
  ): Promise<DurableOutcome<DurableSmMutationResult<DurableSmClearResult>>> {
    return this.transact<DurableSmMutationResult<DurableSmClearResult>>(
      owner.accountKey,
      (account, authorityNow) => {
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
      },
    );
  }
}

export class MemoryDurableOutboundStore extends RuntimeDurableStore {
  private readonly accounts = new Map<string, RuntimeAccount>();
  private transactionTail: Promise<void> = Promise.resolve();

  constructor(
    authorityClock: DurableAuthorityClock = systemAuthorityClock,
    private readonly beforeTransaction?: () => Promise<void>,
  ) {
    super(authorityClock);
  }

  protected transact<T>(
    accountKey: string,
    mutate: (account: RuntimeAccount, authorityNow: number) => AccountMutation<T>,
  ): Promise<DurableOutcome<T>> {
    const operation = this.transactionTail.then(async (): Promise<DurableOutcome<T>> => {
      await this.beforeTransaction?.();
      const existing = this.accounts.get(accountKey);
      const account = existing
        ? decodeRuntimeAccount(cloneValue(existing), accountKey)
        : emptyAccount(accountKey);
      const previousAuthorityTime = account.lastAuthorityTimeMs;
      const {
        authorityNow,
        metadataChanged,
        authorityEpochChanged,
      } = this.sampleAuthorityTime(account);
      const mutation = mutate(account, authorityNow);
      account.lastAuthorityTimeMs = authorityNow;
      if (mutation.changed || authorityEpochChanged) {
        account.revision = checkedDurableCounterIncrement(
          account.revision,
          "Durable account revision",
        );
      }
      const committedValue = mutation.finalize
        ? mutation.finalize(account.revision)
        : mutation.value;
      if (
        mutation.changed
        || metadataChanged
        || authorityNow !== previousAuthorityTime
      ) {
        this.accounts.set(accountKey, account);
      }
      return { kind: "committed", value: cloneValue(committedValue) };
    }).catch((error) => failed<T>(error));
    this.transactionTail = operation.then(() => undefined);
    return operation;
  }
}

export type IndexedDbDurableOutboundStoreOptions = {
  authorityClock?: DurableAuthorityClock;
  databaseName?: string;
  databaseVersion?: number;
  indexedDb?: IDBFactory;
};

function openDatabase(
  indexedDb: IDBFactory | undefined,
  databaseName: string,
  databaseVersion: number,
): Promise<IDBDatabase> {
  return new Promise<IDBDatabase>((resolve, reject) => {
    if (!indexedDb) {
      reject(new DOMException("IndexedDB is unavailable", "NotSupportedError"));
      return;
    }
    let settled = false;
    const rejectOnce = (error: unknown): void => {
      if (settled) return;
      settled = true;
      reject(error);
    };
    const request = indexedDb.open(databaseName, databaseVersion);
    request.onupgradeneeded = () => {
      if (!request.result.objectStoreNames.contains(ACCOUNT_STORE_NAME)) {
        request.result.createObjectStore(ACCOUNT_STORE_NAME, { keyPath: "accountKey" });
      }
    };
    request.onsuccess = () => {
      if (settled) {
        request.result.close();
        return;
      }
      settled = true;
      resolve(request.result);
    };
    request.onerror = () => rejectOnce(
      request.error ?? new Error("IndexedDB open failed"),
    );
    request.onblocked = () => rejectOnce(
      new DOMException("IndexedDB upgrade blocked", "AbortError"),
    );
  });
}

export class IndexedDbDurableOutboundStore extends RuntimeDurableStore {
  private readonly indexedDb: IDBFactory | undefined;
  private readonly databaseName: string;
  private readonly databaseVersion: number;
  private databasePromise: Promise<IDBDatabase> | null = null;

  constructor(options: IndexedDbDurableOutboundStoreOptions = {}) {
    super(options.authorityClock ?? systemAuthorityClock);
    this.indexedDb = options.indexedDb ?? globalThis.indexedDB;
    this.databaseName = options.databaseName ?? DATABASE_NAME;
    this.databaseVersion = options.databaseVersion ?? DATABASE_VERSION;
    if (
      !Number.isSafeInteger(this.databaseVersion)
      || this.databaseVersion < 1
    ) {
      throw new RangeError("IndexedDB databaseVersion must be a positive integer");
    }
  }

  private database(): Promise<IDBDatabase> {
    if (this.databasePromise) return this.databasePromise;
    const opening = openDatabase(
      this.indexedDb,
      this.databaseName,
      this.databaseVersion,
    );
    this.databasePromise = opening;
    void opening.then(
      (database) => {
        database.onversionchange = () => {
          database.close();
          if (this.databasePromise === opening) this.databasePromise = null;
        };
      },
      () => {
        if (this.databasePromise === opening) this.databasePromise = null;
      },
    );
    return opening;
  }

  async close(): Promise<void> {
    const opened = this.databasePromise;
    this.databasePromise = null;
    if (!opened) return;
    try {
      (await opened).close();
    } catch {
      // A failed/blocked open has no connection to close.
    }
  }

  protected async transact<T>(
    accountKey: string,
    mutate: (account: RuntimeAccount, authorityNow: number) => AccountMutation<T>,
  ): Promise<DurableOutcome<T>> {
    let database: IDBDatabase;
    try {
      database = await this.database();
    } catch (error) {
      return failed(error);
    }

    return new Promise((resolve) => {
      let value: T | undefined;
      let valueReady = false;
      let operationError: unknown;
      let transaction: IDBTransaction;
      try {
        transaction = database.transaction(
          ACCOUNT_STORE_NAME,
          "readwrite",
          { durability: "strict" },
        );
        const store = transaction.objectStore(ACCOUNT_STORE_NAME);
        const read = store.get(accountKey);
        read.onsuccess = () => {
          try {
            const account = read.result === undefined
              ? emptyAccount(accountKey)
              : decodeRuntimeAccount(read.result, accountKey);
            const previousAuthorityTime = account.lastAuthorityTimeMs;
            const {
              authorityNow,
              metadataChanged,
              authorityEpochChanged,
            } = this.sampleAuthorityTime(account);
            const mutation = mutate(account, authorityNow);
            account.lastAuthorityTimeMs = authorityNow;
            if (mutation.changed || authorityEpochChanged) {
              account.revision = checkedDurableCounterIncrement(
                account.revision,
                "Durable account revision",
              );
            }
            value = mutation.finalize
              ? mutation.finalize(account.revision)
              : mutation.value;
            valueReady = true;
            if (
              !mutation.changed
              && !metadataChanged
              && authorityNow === previousAuthorityTime
            ) return;
            const write = store.put(account);
            write.onerror = () => {
              operationError = write.error ?? new Error("IndexedDB account write failed");
              transaction.abort();
            };
          } catch (error) {
            operationError = error;
            transaction.abort();
          }
        };
        read.onerror = () => {
          operationError = read.error ?? new Error("IndexedDB account read failed");
          transaction.abort();
        };
      } catch (error) {
        resolve(failed(error));
        return;
      }
      transaction.oncomplete = () => {
        if (!valueReady) {
          resolve(failed(new DOMException("IndexedDB operation did not settle", "AbortError")));
          return;
        }
        resolve({ kind: "committed", value: cloneValue(value as T) });
      };
      transaction.onerror = () => {
        // `onabort` owns the single terminal result.
      };
      transaction.onabort = () => resolve(failed(operationError ?? transaction.error));
    });
  }
}
