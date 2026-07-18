import type { PersistedQueuedMessage } from "../outbound-queue-store";
import type { PersistedSmResumeState } from "../xmpp/sm-resume-types";
import { decodePersistedSmResumeState } from "../xmpp/sm-resume-types";
import {
  DurablePredecessorCapacityError,
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
} from "./durable-contract";
import {
  applyAuthorityClockSample,
  checkedDurableCounterIncrement,
  cloneDurableSmState,
  cloneValue,
  emptyAccount,
  orderKey,
  type AccountMutation,
  type RuntimeAccount,
} from "./durable-model";
import {
  decodeRuntimeAccount,
  outboundPayloadDigest,
} from "./durable-codec";
import {
  applyTerminalTransition,
  claimHeadTransition,
  listOutboundTransition,
  listTerminalTransition,
  persistClaimedTransition,
  persistReadyTransition,
  reconcileResumeClaimsTransition,
  recordTerminalTransition,
  releaseClaimTransition,
  releaseForFreshSessionTransition,
  renewClaimTransition,
  revisionTransition,
  scanAndPruneTransition,
  type PreparedOutboundMessage,
  type ReconcileResumeInput,
} from "./durable-outbound-transitions";
import {
  cancelOwnerHandoffTransition,
  claimOwnerTransition,
  clearSmTransition,
  consumeSmTransition,
  loadSmTransition,
  preparePagehideHandoffTransition,
  renewOwnerTransition,
  saveSmTransition,
} from "./durable-owner-sm-transitions";

export type DurableAccountCommit<T> = {
  readonly account: RuntimeAccount;
  readonly write: boolean;
  readonly value: T;
};

export type DurableAccountTransaction<T> = (
  persisted: unknown | undefined,
) => DurableAccountCommit<T>;

export interface DurableAccountRepository {
  transact<T>(
    accountKey: string,
    run: DurableAccountTransaction<T>,
  ): Promise<T>;
  close(): Promise<void>;
}

export const systemAuthorityClock: DurableAuthorityClock = {
  now: () => Date.now(),
};

function classifyFailure(cause: unknown): DurableFailureReason {
  if (cause instanceof DurablePredecessorCapacityError) return "capacity";
  const name = cause instanceof DOMException || cause instanceof Error
    ? cause.name
    : "";
  if (name === "QuotaExceededError") return "quota";
  if (name === "SecurityError") return "security";
  if (name === "AbortError" || name === "TransactionInactiveError") {
    return "aborted";
  }
  return "unavailable";
}

function failed<T>(cause: unknown): DurableOutcome<T> {
  return { kind: "failed", reason: classifyFailure(cause), cause };
}

export class DurableStoreEngine implements DurableOutboundStore {
  constructor(
    private readonly repository: DurableAccountRepository,
    private readonly authorityClock: DurableAuthorityClock = systemAuthorityClock,
  ) {}

  close(): Promise<void> {
    return this.repository.close();
  }

  revision(accountKey: string): Promise<DurableOutcome<number>> {
    return this.transact(accountKey, (account) => revisionTransition(account));
  }

  list(accountKey: string): Promise<DurableOutcome<PersistedQueuedMessage[]>> {
    return this.transact(accountKey, (account) => listOutboundTransition(account));
  }

  scanAndPrune(
    accountKey: string,
    cutoff: number,
  ): Promise<DurableOutcome<DurableOutboundScan>> {
    return this.transact(accountKey, (account, authorityNow) => (
      scanAndPruneTransition(account, authorityNow, cutoff)
    ));
  }

  async persistReady(
    accountKey: string,
    message: PersistedQueuedMessage,
  ): Promise<DurableOutcome<OutboundPersistResult>> {
    try {
      const prepared = await this.prepareOutboundMessage(accountKey, message);
      return await this.transact(accountKey, (account) => (
        persistReadyTransition(account, prepared)
      ));
    } catch (error) {
      return failed(error);
    }
  }

  async persistClaimed(
    accountKey: string,
    message: PersistedQueuedMessage,
    request: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundPersistClaimedResult>> {
    try {
      const prepared = await this.prepareOutboundMessage(accountKey, message);
      return await this.transact(accountKey, (account, authorityNow) => (
        persistClaimedTransition(account, authorityNow, prepared, request)
      ));
    } catch (error) {
      return failed(error);
    }
  }

  claimHead(
    accountKey: string,
    lane: OutboundLane,
    request: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundClaimHeadResult>> {
    return this.transact(accountKey, (account, authorityNow) => (
      claimHeadTransition(account, authorityNow, lane, request)
    ));
  }

  renew(
    identity: OutboundRowIdentity,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<OutboundRenewResult>> {
    return this.transact(identity.accountKey, (account, authorityNow) => (
      renewClaimTransition(account, authorityNow, identity, expected)
    ));
  }

  release(
    identity: OutboundRowIdentity,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<OutboundReleaseResult>> {
    return this.transact(identity.accountKey, (account, authorityNow) => (
      releaseClaimTransition(account, authorityNow, identity, expected)
    ));
  }

  async reconcileResumeClaims(
    owner: OutboundOwnerContext,
    connectionGeneration: number,
    authoritativeMessageIds: readonly string[] | null,
    phase: Extract<OutboundClaimPhase, "resume-replay" | "fresh-fallback">,
  ): Promise<DurableOutcome<ResumeClaimReconciliation>> {
    try {
      const messageIds = authoritativeMessageIds === null
        ? null
        : [...authoritativeMessageIds];
      const claimIds = new Map(
        (messageIds ?? []).map((messageId) => [
          messageId,
          crypto.randomUUID(),
        ]),
      );
      const input: ReconcileResumeInput = {
        owner,
        connectionGeneration,
        authoritativeMessageIds: messageIds,
        phase,
        claimIds,
      };
      return await this.transact(
        owner.accountKey,
        (account, authorityNow) => (
          reconcileResumeClaimsTransition(account, authorityNow, input)
        ),
      );
    } catch (error) {
      return failed(error);
    }
  }

  releaseForFreshSession(
    owner: OutboundOwnerContext,
    connectionGeneration: number,
  ): Promise<DurableOutcome<string[] | null>> {
    return this.transact(owner.accountKey, (account, authorityNow) => (
      releaseForFreshSessionTransition(
        account,
        authorityNow,
        owner,
        connectionGeneration,
      )
    ));
  }

  listTerminal(
    accountKey: string,
  ): Promise<DurableOutcome<OutboundTerminalIntent[]>> {
    return this.transact(accountKey, (account) => listTerminalTransition(account));
  }

  async recordTerminal(
    identity: OutboundRowIdentity,
    kind: OutboundTerminalKind,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<OutboundTerminalRecordResult>> {
    try {
      const intentId = crypto.randomUUID();
      return await this.transact(
        identity.accountKey,
        (account, authorityNow) => (
          recordTerminalTransition(
            account,
            authorityNow,
            identity,
            kind,
            expected,
            intentId,
          )
        ),
      );
    } catch (error) {
      return failed(error);
    }
  }

  applyTerminal(
    executor: OutboundOwnerContext,
    intent: OutboundTerminalIntent,
  ): Promise<DurableOutcome<OutboundTerminalApplyResult>> {
    return this.transact(intent.accountKey, (account, authorityNow) => (
      applyTerminalTransition(account, authorityNow, executor, intent)
    ));
  }

  async claimOwner(
    accountKey: string,
    hint: OutboundOwnerHint,
  ): Promise<DurableOutcome<OutboundOwnerActivation>> {
    try {
      const rotatedOwnerId = crypto.randomUUID();
      return await this.transact(accountKey, (account, authorityNow) => (
        claimOwnerTransition(account, authorityNow, hint, rotatedOwnerId)
      ));
    } catch (error) {
      return failed(error);
    }
  }

  renewOwner(
    owner: OutboundOwnerContext,
  ): Promise<DurableOutcome<boolean>> {
    return this.transact(owner.accountKey, (account, authorityNow) => (
      renewOwnerTransition(account, authorityNow, owner)
    ));
  }

  async preparePagehideHandoff(
    owner: OutboundOwnerContext,
    expectedSmVersion: number | null,
    handoffToken: string,
    state: PersistedSmResumeState | null,
  ): Promise<DurableOutcome<DurableSmMutationResult<PagehideHandoffResult>>> {
    try {
      const snapshot = state === null
        ? null
        : cloneDurableSmState(
            decodePersistedSmResumeState(state, "pagehide.state"),
          );
      return await this.transact(
        owner.accountKey,
        (account, authorityNow) => (
          preparePagehideHandoffTransition(
            account,
            authorityNow,
            owner,
            expectedSmVersion,
            handoffToken,
            snapshot,
          )
        ),
      );
    } catch (error) {
      return failed(error);
    }
  }

  cancelOwnerHandoff(
    owner: OutboundOwnerContext,
    expectedToken: string,
    expectedSmVersion: number,
  ): Promise<DurableOutcome<PagehideHandoffCancelResult>> {
    return this.transact(owner.accountKey, (account, authorityNow) => (
      cancelOwnerHandoffTransition(
        account,
        authorityNow,
        owner,
        expectedToken,
        expectedSmVersion,
      )
    ));
  }

  loadSm(
    owner: OutboundOwnerContext,
  ): Promise<DurableOutcome<DurableSmLoadResult>> {
    return this.transact(owner.accountKey, (account, authorityNow) => (
      loadSmTransition(account, authorityNow, owner)
    ));
  }

  consumeSm(
    owner: OutboundOwnerContext,
    expectedVersion: number | null,
    usable: (envelope: DurableSmEnvelope) => boolean,
  ): Promise<DurableOutcome<DurableSmMutationResult<DurableSmEnvelope | null>>> {
    return this.transact(owner.accountKey, (account, authorityNow) => (
      consumeSmTransition(
        account,
        authorityNow,
        owner,
        expectedVersion,
        usable,
      )
    ));
  }

  async saveSm(
    owner: OutboundOwnerContext,
    expectedVersion: number | null,
    state: PersistedSmResumeState,
    _savedAt: number,
  ): Promise<DurableOutcome<DurableSmMutationResult<DurableSmEnvelope>>> {
    try {
      const snapshot = cloneDurableSmState(
        decodePersistedSmResumeState(state, "save.state"),
      );
      return await this.transact(owner.accountKey, (account, authorityNow) => (
        saveSmTransition(
          account,
          authorityNow,
          owner,
          expectedVersion,
          snapshot,
        )
      ));
    } catch (error) {
      return failed(error);
    }
  }

  clearSm(
    owner: OutboundOwnerContext,
    expectedVersion: number | null,
  ): Promise<DurableOutcome<DurableSmMutationResult<DurableSmClearResult>>> {
    return this.transact(owner.accountKey, (account, authorityNow) => (
      clearSmTransition(account, authorityNow, owner, expectedVersion)
    ));
  }

  private async transact<T>(
    accountKey: string,
    mutate: (
      account: RuntimeAccount,
      authorityNow: number,
    ) => AccountMutation<T>,
  ): Promise<DurableOutcome<T>> {
    try {
      const value = await this.repository.transact(
        accountKey,
        (persisted): DurableAccountCommit<T> => {
          const account = persisted === undefined
            ? emptyAccount(accountKey)
            : decodeRuntimeAccount(persisted, accountKey);
          const previousAuthorityTime = account.lastAuthorityTimeMs;
          const wallClockNow = this.authorityClock.now();
          const {
            authorityNow,
            metadataChanged,
            authorityEpochChanged,
          } = applyAuthorityClockSample(account, wallClockNow);
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
          return {
            account,
            write: mutation.changed
              || metadataChanged
              || authorityNow !== previousAuthorityTime,
            value: cloneValue(committedValue),
          };
        },
      );
      return { kind: "committed", value };
    } catch (error) {
      return failed(error);
    }
  }

  private async prepareOutboundMessage(
    accountKey: string,
    message: PersistedQueuedMessage,
  ): Promise<PreparedOutboundMessage> {
    const durableMessage = cloneValue(message);
    const payloadDigest = await outboundPayloadDigest(durableMessage);
    return {
      identity: {
        accountKey,
        messageId: durableMessage.id,
        incarnation: crypto.randomUUID(),
        payloadDigest,
      },
      lane: outboundLane(durableMessage),
      orderKey: orderKey(durableMessage),
      message: durableMessage,
    };
  }
}
