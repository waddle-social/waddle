import type { PersistedQueuedMessage } from "./outbound-queue-store";
import type { PersistedSmResumeState } from "./xmpp/sm-resume-types";
import { decodePersistedSmResumeState } from "./xmpp/sm-resume-types";
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
} from "./xmpp-runtime/durable-contract";
import {
  AUTHORITY_CLOCK_ROLLBACK_TOLERANCE_MS,
  checkedDurableCounterIncrement,
  cloneDurableSmState,
  cloneValue,
  emptyAccount,
  orderKey,
  type AccountMutation,
  type RuntimeAccount,
} from "./xmpp-runtime/durable-model";
import {
  decodeRuntimeAccount,
  outboundPayloadDigest,
} from "./xmpp-runtime/durable-codec";
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
} from "./xmpp-runtime/durable-outbound-transitions";
import {
  cancelOwnerHandoffTransition,
  claimOwnerTransition,
  clearSmTransition,
  consumeSmTransition,
  loadSmTransition,
  preparePagehideHandoffTransition,
  renewOwnerTransition,
  saveSmTransition,
} from "./xmpp-runtime/durable-owner-sm-transitions";

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

async function prepareOutboundMessage(
  accountKey: string,
  message: PersistedQueuedMessage,
): Promise<PreparedOutboundMessage> {
  const durableMessage = cloneValue(message);
  return {
    identity: {
      accountKey,
      messageId: durableMessage.id,
      incarnation: crypto.randomUUID(),
      payloadDigest: await outboundPayloadDigest(durableMessage),
    },
    lane: outboundLane(durableMessage),
    orderKey: orderKey(durableMessage),
    message: durableMessage,
  };
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
    let prepared: PreparedOutboundMessage;
    try {
      prepared = await prepareOutboundMessage(accountKey, message);
    } catch (error) {
      return failed(error);
    }
    return this.transact(accountKey, (account) => (
      persistReadyTransition(account, prepared)
    ));
  }

  async persistClaimed(
    accountKey: string,
    message: PersistedQueuedMessage,
    request: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundPersistClaimedResult>> {
    let prepared: PreparedOutboundMessage;
    try {
      prepared = await prepareOutboundMessage(accountKey, message);
    } catch (error) {
      return failed(error);
    }
    return this.transact(accountKey, (account, authorityNow) => (
      persistClaimedTransition(account, authorityNow, prepared, request)
    ));
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
    const claimIds = new Map(
      (authoritativeMessageIds ?? []).map((messageId) => [
        messageId,
        crypto.randomUUID(),
      ]),
    );
    const input: ReconcileResumeInput = {
      owner,
      connectionGeneration,
      authoritativeMessageIds,
      phase,
      claimIds,
    };
    return this.transact(owner.accountKey, (account, authorityNow) => (
      reconcileResumeClaimsTransition(account, authorityNow, input)
    ));
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

  recordTerminal(
    identity: OutboundRowIdentity,
    kind: OutboundTerminalKind,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<OutboundTerminalRecordResult>> {
    const intentId = crypto.randomUUID();
    return this.transact(identity.accountKey, (account, authorityNow) => (
      recordTerminalTransition(
        account,
        authorityNow,
        identity,
        kind,
        expected,
        intentId,
      )
    ));
  }

  applyTerminal(
    executor: OutboundOwnerContext,
    intent: OutboundTerminalIntent,
  ): Promise<DurableOutcome<OutboundTerminalApplyResult>> {
    return this.transact(intent.accountKey, (account, authorityNow) => (
      applyTerminalTransition(account, authorityNow, executor, intent)
    ));
  }

  claimOwner(
    accountKey: string,
    hint: OutboundOwnerHint,
  ): Promise<DurableOutcome<OutboundOwnerActivation>> {
    const rotatedOwnerId = crypto.randomUUID();
    return this.transact(accountKey, (account, authorityNow) => (
      claimOwnerTransition(account, authorityNow, hint, rotatedOwnerId)
    ));
  }

  renewOwner(
    owner: OutboundOwnerContext,
  ): Promise<DurableOutcome<boolean>> {
    return this.transact(owner.accountKey, (account, authorityNow) => (
      renewOwnerTransition(account, authorityNow, owner)
    ));
  }

  preparePagehideHandoff(
    owner: OutboundOwnerContext,
    expectedSmVersion: number | null,
    handoffToken: string,
    state: PersistedSmResumeState | null,
  ): Promise<DurableOutcome<DurableSmMutationResult<PagehideHandoffResult>>> {
    let snapshot: PersistedSmResumeState | null;
    try {
      snapshot = state === null
        ? null
        : cloneDurableSmState(
            decodePersistedSmResumeState(state, "pagehide.state"),
          );
    } catch (error) {
      return Promise.resolve(failed(error));
    }
    return this.transact(owner.accountKey, (account, authorityNow) => (
      preparePagehideHandoffTransition(
        account,
        authorityNow,
        owner,
        expectedSmVersion,
        handoffToken,
        snapshot,
      )
    ));
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

  saveSm(
    owner: OutboundOwnerContext,
    expectedVersion: number | null,
    state: PersistedSmResumeState,
    _savedAt: number,
  ): Promise<DurableOutcome<DurableSmMutationResult<DurableSmEnvelope>>> {
    let snapshot: PersistedSmResumeState;
    try {
      snapshot = cloneDurableSmState(
        decodePersistedSmResumeState(state, "save.state"),
      );
    } catch (error) {
      return Promise.resolve(failed(error));
    }
    return this.transact(owner.accountKey, (account, authorityNow) => (
      saveSmTransition(
        account,
        authorityNow,
        owner,
        expectedVersion,
        snapshot,
      )
    ));
  }

  clearSm(
    owner: OutboundOwnerContext,
    expectedVersion: number | null,
  ): Promise<DurableOutcome<DurableSmMutationResult<DurableSmClearResult>>> {
    return this.transact(owner.accountKey, (account, authorityNow) => (
      clearSmTransition(account, authorityNow, owner, expectedVersion)
    ));
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
