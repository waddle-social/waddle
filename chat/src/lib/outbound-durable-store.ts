import type { PersistedQueuedMessage } from "./outbound-queue-store";

const DATABASE_NAME = "waddle-chat-outbound";
const DATABASE_VERSION = 2;
const STORE_NAME = "messages";
const TERMINAL_STORE_NAME = "terminal-mutations";
const OWNER_STORE_NAME = "owners";

export const OUTBOUND_CLAIM_LEASE_MS = 45_000;

export type OutboundClaimPhase =
  | "sending"
  | "resume-replay"
  | "fresh-fallback";

export type OutboundClaimRequest = {
  ownerId: string;
  ownerInstanceId: string;
  ownerGeneration: number;
  connectionGeneration: number;
  claimId: string;
  phase: OutboundClaimPhase;
  leaseUntil: number;
};

export type OutboundClaim = OutboundClaimRequest & {
  rowIncarnation: string;
};

export type OutboundOwnerContext = {
  ownerId: string;
  ownerInstanceId: string;
  ownerGeneration: number;
};

export type OutboundOwnerHint = {
  ownerId: string;
  ownerInstanceId: string;
  handoffToken?: string;
};

export type OutboundOwnerHandoff = {
  token: string;
  expiresAt: number;
};

export type OutboundClaimResult =
  | { kind: "claimed"; claim: OutboundClaim }
  | { kind: "busy"; claim: OutboundClaim }
  | { kind: "missing" }
  | { kind: "terminal" };

export type OutboundTerminalKind = "ack" | "failure" | "nonretryable";

export type OutboundTerminalIntent = {
  key: string;
  accountKey: string;
  messageId: string;
  kind: OutboundTerminalKind;
  expected: OutboundClaim;
  recordedAt: number;
};

export type OutboundTerminalApplyResult =
  | { kind: "acked" }
  | { kind: "removed" }
  | { kind: "released" }
  | { kind: "fallback"; claim: OutboundClaim }
  | { kind: "missing" }
  | { kind: "stale" };

export type OutboundTerminalRecordResult =
  | { kind: "recorded"; intent: OutboundTerminalIntent }
  | { kind: "missing" }
  | { kind: "stale" };

export type DurableOutboundScan = {
  messages: PersistedQueuedMessage[];
  prunedIds: string[];
};

type DurableOutboundRow = {
  key: string;
  accountKey: string;
  incarnation: string;
  message: PersistedQueuedMessage;
  claim?: OutboundClaim;
};

type DurableOutboundOwner = {
  key: string;
  ownerId: string;
  ownerInstanceId: string;
  ownerGeneration: number;
  leaseUntil: number;
  handoff?: OutboundOwnerHandoff;
  predecessor?: {
    ownerInstanceId: string;
    ownerGeneration: number;
    expiresAt: number;
  };
};

type DurableFailureReason =
  | "unavailable"
  | "quota"
  | "security"
  | "aborted";

export type DurableOutcome<T> =
  | { kind: "committed"; value: T }
  | { kind: "failed"; reason: DurableFailureReason; cause?: unknown };

class OutboundPersistenceError extends Error {
  constructor(
    readonly operation: string,
    readonly reason: DurableFailureReason,
    readonly cause?: unknown,
  ) {
    super(`Outbound persistence ${operation} failed: ${reason}`);
    this.name = "OutboundPersistenceError";
  }
}

export interface DurableOutboundStore {
  list(accountKey: string): Promise<DurableOutcome<PersistedQueuedMessage[]>>;
  scanAndPrune(accountKey: string, cutoff: number): Promise<DurableOutcome<DurableOutboundScan>>;
  persistReady(accountKey: string, message: PersistedQueuedMessage): Promise<DurableOutcome<void>>;
  persistClaimed(
    accountKey: string,
    message: PersistedQueuedMessage,
    claim: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundClaim>>;
  claim(
    accountKey: string,
    messageId: string,
    claim: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundClaimResult>>;
  adopt(
    accountKey: string,
    messageId: string,
    claim: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundClaimResult>>;
  renew(
    accountKey: string,
    messageId: string,
    expected: OutboundClaim,
    leaseUntil: number,
  ): Promise<DurableOutcome<OutboundClaim | null>>;
  transition(
    accountKey: string,
    messageId: string,
    expected: OutboundClaim,
    phase: OutboundClaimPhase,
  ): Promise<DurableOutcome<OutboundClaim | null>>;
  release(
    accountKey: string,
    messageId: string,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<boolean>>;
  deleteOwned(
    accountKey: string,
    messageId: string,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<boolean>>;
  listTerminal(accountKey: string): Promise<DurableOutcome<OutboundTerminalIntent[]>>;
  recordTerminal(
    accountKey: string,
    messageId: string,
    kind: OutboundTerminalKind,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<OutboundTerminalRecordResult>>;
  applyTerminal(intent: OutboundTerminalIntent): Promise<DurableOutcome<OutboundTerminalApplyResult>>;
  claimOwner(hint: OutboundOwnerHint): Promise<DurableOutcome<OutboundOwnerContext>>;
  renewOwner(owner: OutboundOwnerContext, leaseUntil: number): Promise<DurableOutcome<boolean>>;
  prepareOwnerHandoff(
    owner: OutboundOwnerContext,
    handoff: OutboundOwnerHandoff,
  ): Promise<DurableOutcome<boolean>>;
  cancelOwnerHandoff(owner: OutboundOwnerContext): Promise<DurableOutcome<boolean>>;
}

function rowKey(accountKey: string, messageId: string): string {
  return `${accountKey.length}:${accountKey}:${messageId}`;
}

function classifyFailure(cause: unknown): DurableFailureReason {
  const name = cause instanceof DOMException || cause instanceof Error ? cause.name : "";
  if (name === "QuotaExceededError") return "quota";
  if (name === "SecurityError") return "security";
  if (name === "AbortError" || name === "TransactionInactiveError") return "aborted";
  return "unavailable";
}

function failed<T>(cause: unknown): DurableOutcome<T> {
  return { kind: "failed", reason: classifyFailure(cause), cause };
}

function sameClaim(left: OutboundClaim | undefined, right: OutboundClaim): boolean {
  return !!left
    && left.ownerId === right.ownerId
    && left.ownerInstanceId === right.ownerInstanceId
    && left.ownerGeneration === right.ownerGeneration
    && left.connectionGeneration === right.connectionGeneration
    && left.claimId === right.claimId
    && left.rowIncarnation === right.rowIncarnation;
}

let databasePromise: Promise<IDBDatabase> | null = null;

function openDatabase(): Promise<IDBDatabase> {
  if (databasePromise) return databasePromise;
  const opening = new Promise<IDBDatabase>((resolve, reject) => {
    if (typeof indexedDB === "undefined") {
      reject(new DOMException("IndexedDB is unavailable", "NotSupportedError"));
      return;
    }
    const request = indexedDB.open(DATABASE_NAME, DATABASE_VERSION);
    request.onupgradeneeded = () => {
      if (request.oldVersion < DATABASE_VERSION) {
        for (const storeName of [STORE_NAME, TERMINAL_STORE_NAME, OWNER_STORE_NAME]) {
          if (request.result.objectStoreNames.contains(storeName)) {
            request.result.deleteObjectStore(storeName);
          }
        }
      }
      if (!request.result.objectStoreNames.contains(STORE_NAME)) {
        request.result.createObjectStore(STORE_NAME, { keyPath: "key" });
      }
      if (!request.result.objectStoreNames.contains(TERMINAL_STORE_NAME)) {
        request.result.createObjectStore(TERMINAL_STORE_NAME, { keyPath: "key" });
      }
      if (!request.result.objectStoreNames.contains(OWNER_STORE_NAME)) {
        request.result.createObjectStore(OWNER_STORE_NAME, { keyPath: "key" });
      }
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error("IndexedDB open failed"));
    request.onblocked = () => reject(new DOMException("IndexedDB upgrade blocked", "AbortError"));
  }).catch((error) => {
    databasePromise = null;
    throw error;
  });
  databasePromise = opening;
  return opening;
}

async function writeTransaction<T>(
  mutate: (
    messages: IDBObjectStore,
    terminals: IDBObjectStore,
    owners: IDBObjectStore,
    settle: (value: T) => void,
    abort: (cause: unknown) => void,
  ) => void,
): Promise<DurableOutcome<T>> {
  let database: IDBDatabase;
  try {
    database = await openDatabase();
  } catch (error) {
    return failed(error);
  }

  return new Promise((resolve) => {
    let result: T | undefined;
    let resultReady = false;
    let operationError: unknown;
    let transaction: IDBTransaction;
    try {
      transaction = database.transaction(
        [STORE_NAME, TERMINAL_STORE_NAME, OWNER_STORE_NAME],
        "readwrite",
        { durability: "strict" },
      );
      mutate(
        transaction.objectStore(STORE_NAME),
        transaction.objectStore(TERMINAL_STORE_NAME),
        transaction.objectStore(OWNER_STORE_NAME),
        (value) => {
          result = value;
          resultReady = true;
        },
        (cause) => {
          operationError = cause;
          transaction.abort();
        },
      );
    } catch (error) {
      resolve(failed(error));
      return;
    }
    transaction.oncomplete = () => {
      if (!resultReady) {
        resolve(failed(new DOMException("IndexedDB operation did not settle", "AbortError")));
        return;
      }
      resolve({ kind: "committed", value: result as T });
    };
    transaction.onerror = () => {
      // `onabort` owns the single terminal result.
    };
    transaction.onabort = () => resolve(failed(operationError ?? transaction.error));
  });
}

function terminalIntentKey(accountKey: string, messageId: string): string {
  return rowKey(accountKey, messageId);
}

function readTerminal(
  request: IDBRequest,
  onSuccess: (intent: OutboundTerminalIntent | undefined) => void,
  abort: (cause: unknown) => void,
): void {
  request.onsuccess = () => onSuccess(request.result as OutboundTerminalIntent | undefined);
  request.onerror = () => abort(request.error ?? new Error("IndexedDB terminal intent read failed"));
}

function readOwner(
  request: IDBRequest,
  onSuccess: (owner: DurableOutboundOwner | undefined) => void,
  abort: (cause: unknown) => void,
): void {
  request.onsuccess = () => onSuccess(request.result as DurableOutboundOwner | undefined);
  request.onerror = () => abort(request.error ?? new Error("IndexedDB owner read failed"));
}

function putOwner(
  store: IDBObjectStore,
  owner: DurableOutboundOwner,
  settle: (value: OutboundOwnerContext) => void,
  abort: (cause: unknown) => void,
): void {
  const request = store.put(owner);
  request.onsuccess = () => settle(ownerContext(owner));
  request.onerror = () => abort(request.error ?? new Error("IndexedDB owner write failed"));
}

function ownerContext(owner: DurableOutboundOwner): OutboundOwnerContext {
  return {
    ownerId: owner.ownerId,
    ownerInstanceId: owner.ownerInstanceId,
    ownerGeneration: owner.ownerGeneration,
  };
}

function claimForRow(request: OutboundClaimRequest, rowIncarnation: string): OutboundClaim {
  return { ...request, rowIncarnation };
}

function readRow(request: IDBRequest, onSuccess: (row: DurableOutboundRow | undefined) => void, abort: (cause: unknown) => void): void {
  request.onsuccess = () => onSuccess(request.result as DurableOutboundRow | undefined);
  request.onerror = () => abort(request.error ?? new Error("IndexedDB read failed"));
}

export class IndexedDbDurableOutboundStore implements DurableOutboundStore {
  list(accountKey: string): Promise<DurableOutcome<PersistedQueuedMessage[]>> {
    return writeTransaction((store, _terminals, _owners, settle, abort) => {
      const messages: PersistedQueuedMessage[] = [];
      const request = store.openCursor();
      request.onsuccess = () => {
        const cursor = request.result;
        if (!cursor) {
          settle(messages);
          return;
        }
        const row = cursor.value as DurableOutboundRow;
        if (row.accountKey === accountKey) messages.push(row.message);
        cursor.continue();
      };
      request.onerror = () => abort(request.error ?? new Error("IndexedDB cursor failed"));
    });
  }

  scanAndPrune(accountKey: string, cutoff: number): Promise<DurableOutcome<DurableOutboundScan>> {
    return writeTransaction((store, terminals, _owners, settle, abort) => {
      const messages: PersistedQueuedMessage[] = [];
      const prunedIds: string[] = [];
      const now = Date.now();
      const request = store.openCursor();
      request.onsuccess = () => {
        const cursor = request.result;
        if (!cursor) {
          settle({ messages, prunedIds });
          return;
        }
        const row = cursor.value as DurableOutboundRow;
        if (row.accountKey !== accountKey) {
          cursor.continue();
          return;
        }
        const createdAt = Date.parse(row.message.createdAt);
        const stale = Number.isFinite(createdAt) && createdAt < cutoff;
        const liveClaim = !!row.claim && row.claim.leaseUntil > now;
        if (!stale || liveClaim) {
          messages.push(row.message);
          cursor.continue();
          return;
        }
        readTerminal(terminals.get(row.key), (terminal) => {
          if (terminal) {
            messages.push(row.message);
            cursor.continue();
            return;
          }
          const deletion = cursor.delete();
          deletion.onsuccess = () => {
            prunedIds.push(row.message.id);
            cursor.continue();
          };
          deletion.onerror = () => abort(deletion.error ?? new Error("IndexedDB TTL prune failed"));
        }, abort);
      };
      request.onerror = () => abort(request.error ?? new Error("IndexedDB scan failed"));
    });
  }

  persistReady(accountKey: string, message: PersistedQueuedMessage): Promise<DurableOutcome<void>> {
    const key = rowKey(accountKey, message.id);
    return writeTransaction((store, terminals, _owners, settle, abort) => {
      readTerminal(terminals.get(key), (terminal) => {
        if (terminal) {
          abort(new DOMException("Message has a pending terminal mutation", "AbortError"));
          return;
        }
        readRow(store.get(key), (existing) => {
          if (existing?.claim && existing.claim.leaseUntil > Date.now()) {
            abort(new DOMException("Message is claimed by another connection", "AbortError"));
            return;
          }
          const request = store.put({
            key,
            accountKey,
            incarnation: crypto.randomUUID(),
            message,
          } satisfies DurableOutboundRow);
          request.onsuccess = () => settle(undefined);
          request.onerror = () => abort(request.error ?? new Error("IndexedDB put failed"));
        }, abort);
      }, abort);
    });
  }

  persistClaimed(
    accountKey: string,
    message: PersistedQueuedMessage,
    requestClaim: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundClaim>> {
    const key = rowKey(accountKey, message.id);
    return writeTransaction((store, terminals, _owners, settle, abort) => {
      readTerminal(terminals.get(key), (terminal) => {
        if (terminal) {
          abort(new DOMException("Message has a pending terminal mutation", "AbortError"));
          return;
        }
        readRow(store.get(key), (existing) => {
          if (existing?.claim && existing.claim.leaseUntil > Date.now()) {
            abort(new DOMException("Message is claimed by another connection", "AbortError"));
            return;
          }
          const incarnation = crypto.randomUUID();
          const claim = claimForRow(requestClaim, incarnation);
          const request = store.put({
            key,
            accountKey,
            incarnation,
            message,
            claim,
          } satisfies DurableOutboundRow);
          request.onsuccess = () => settle(claim);
          request.onerror = () => abort(request.error ?? new Error("IndexedDB put failed"));
        }, abort);
      }, abort);
    });
  }

  claim(
    accountKey: string,
    messageId: string,
    requestClaim: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundClaimResult>> {
    const key = rowKey(accountKey, messageId);
    return writeTransaction((store, terminals, _owners, settle, abort) => {
      readTerminal(terminals.get(key), (terminal) => {
        if (terminal) {
          settle({ kind: "terminal" });
          return;
        }
        readRow(store.get(key), (row) => {
          if (!row) {
            settle({ kind: "missing" });
            return;
          }
          if (row.claim && row.claim.leaseUntil > Date.now()) {
            settle({ kind: "busy", claim: row.claim });
            return;
          }
          const claim = claimForRow(requestClaim, row.incarnation);
          const request = store.put({ ...row, claim } satisfies DurableOutboundRow);
          request.onsuccess = () => settle({ kind: "claimed", claim });
          request.onerror = () => abort(request.error ?? new Error("IndexedDB claim failed"));
        }, abort);
      }, abort);
    });
  }

  adopt(
    accountKey: string,
    messageId: string,
    requestClaim: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundClaimResult>> {
    const key = rowKey(accountKey, messageId);
    return writeTransaction((store, terminals, owners, settle, abort) => {
      readTerminal(terminals.get(key), (terminal) => {
        if (terminal) {
          settle({ kind: "terminal" });
          return;
        }
        readRow(store.get(key), (row) => {
          if (!row) {
            settle({ kind: "missing" });
            return;
          }
          const now = Date.now();
          if (!row.claim || row.claim.leaseUntil <= now) {
            const claim = claimForRow(requestClaim, row.incarnation);
            const request = store.put({ ...row, claim } satisfies DurableOutboundRow);
            request.onsuccess = () => settle({ kind: "claimed", claim });
            request.onerror = () => abort(request.error ?? new Error("IndexedDB adoption failed"));
            return;
          }
          readOwner(owners.get(requestClaim.ownerId), (owner) => {
            const predecessor = owner?.predecessor;
            const authorized = !!owner
              && owner.ownerInstanceId === requestClaim.ownerInstanceId
              && owner.ownerGeneration === requestClaim.ownerGeneration
              && owner.leaseUntil > now
              && !!predecessor
              && predecessor.expiresAt > now
              && row.claim?.ownerId === owner.ownerId
              && row.claim.ownerInstanceId === predecessor.ownerInstanceId
              && row.claim.ownerGeneration === predecessor.ownerGeneration;
            if (!authorized) {
              settle({ kind: "busy", claim: row.claim! });
              return;
            }
            const claim = claimForRow(requestClaim, row.incarnation);
            const request = store.put({ ...row, claim } satisfies DurableOutboundRow);
            request.onsuccess = () => settle({ kind: "claimed", claim });
            request.onerror = () => abort(request.error ?? new Error("IndexedDB adoption failed"));
          }, abort);
        }, abort);
      }, abort);
    });
  }

  renew(
    accountKey: string,
    messageId: string,
    expected: OutboundClaim,
    leaseUntil: number,
  ): Promise<DurableOutcome<OutboundClaim | null>> {
    return this.mutateClaim(accountKey, messageId, expected, (claim) => ({ ...claim, leaseUntil }));
  }

  transition(
    accountKey: string,
    messageId: string,
    expected: OutboundClaim,
    phase: OutboundClaimPhase,
  ): Promise<DurableOutcome<OutboundClaim | null>> {
    return this.mutateClaim(accountKey, messageId, expected, (claim) => ({
      ...claim,
      phase,
      leaseUntil: Date.now() + OUTBOUND_CLAIM_LEASE_MS,
    }));
  }

  release(
    accountKey: string,
    messageId: string,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<boolean>> {
    const key = rowKey(accountKey, messageId);
    return writeTransaction((store, _terminals, _owners, settle, abort) => {
      readRow(store.get(key), (row) => {
        if (!row || !sameClaim(row.claim, expected)) {
          settle(false);
          return;
        }
        const { claim: _claim, ...ready } = row;
        const request = store.put(ready satisfies DurableOutboundRow);
        request.onsuccess = () => settle(true);
        request.onerror = () => abort(request.error ?? new Error("IndexedDB release failed"));
      }, abort);
    });
  }

  deleteOwned(
    accountKey: string,
    messageId: string,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<boolean>> {
    const key = rowKey(accountKey, messageId);
    return writeTransaction((store, _terminals, _owners, settle, abort) => {
      readRow(store.get(key), (row) => {
        if (!row || !sameClaim(row.claim, expected)) {
          settle(false);
          return;
        }
        const request = store.delete(key);
        request.onsuccess = () => settle(true);
        request.onerror = () => abort(request.error ?? new Error("IndexedDB delete failed"));
      }, abort);
    });
  }

  listTerminal(accountKey: string): Promise<DurableOutcome<OutboundTerminalIntent[]>> {
    return writeTransaction((_store, terminals, _owners, settle, abort) => {
      const intents: OutboundTerminalIntent[] = [];
      const request = terminals.openCursor();
      request.onsuccess = () => {
        const cursor = request.result;
        if (!cursor) {
          settle(intents);
          return;
        }
        const intent = cursor.value as OutboundTerminalIntent;
        if (intent.accountKey === accountKey) intents.push(intent);
        cursor.continue();
      };
      request.onerror = () => abort(request.error ?? new Error("IndexedDB terminal cursor failed"));
    });
  }

  recordTerminal(
    accountKey: string,
    messageId: string,
    kind: OutboundTerminalKind,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<OutboundTerminalRecordResult>> {
    const key = terminalIntentKey(accountKey, messageId);
    return writeTransaction((store, terminals, _owners, settle, abort) => {
      readRow(store.get(key), (row) => {
        if (!row) {
          settle({ kind: "missing" });
          return;
        }
        if (!sameClaim(row.claim, expected) || row.incarnation !== expected.rowIncarnation) {
          settle({ kind: "stale" });
          return;
        }
        readTerminal(terminals.get(key), (existing) => {
          if (existing) {
            settle({ kind: "recorded", intent: existing });
            return;
          }
          const intent: OutboundTerminalIntent = {
            key,
            accountKey,
            messageId,
            kind,
            expected,
            recordedAt: Date.now(),
          };
          const request = terminals.put(intent);
          request.onsuccess = () => settle({ kind: "recorded", intent });
          request.onerror = () => abort(request.error ?? new Error("IndexedDB terminal intent write failed"));
        }, abort);
      }, abort);
    });
  }

  applyTerminal(intent: OutboundTerminalIntent): Promise<DurableOutcome<OutboundTerminalApplyResult>> {
    return writeTransaction((store, terminals, _owners, settle, abort) => {
      readTerminal(terminals.get(intent.key), (persisted) => {
        if (!persisted) {
          settle({ kind: "missing" });
          return;
        }
        readRow(store.get(intent.key), (row) => {
          if (!row) {
            const deletion = terminals.delete(intent.key);
            deletion.onsuccess = () => settle({ kind: "missing" });
            deletion.onerror = () => abort(deletion.error ?? new Error("IndexedDB terminal cleanup failed"));
            return;
          }
          if (!sameClaim(row.claim, persisted.expected) || row.incarnation !== persisted.expected.rowIncarnation) {
            const deletion = terminals.delete(intent.key);
            deletion.onsuccess = () => settle({ kind: "stale" });
            deletion.onerror = () => abort(deletion.error ?? new Error("IndexedDB stale terminal cleanup failed"));
            return;
          }
          const finish = (result: OutboundTerminalApplyResult) => {
            const deletion = terminals.delete(intent.key);
            deletion.onsuccess = () => settle(result);
            deletion.onerror = () => abort(deletion.error ?? new Error("IndexedDB terminal intent delete failed"));
          };
          if (persisted.kind === "ack" || persisted.kind === "nonretryable") {
            const deletion = store.delete(intent.key);
            deletion.onsuccess = () => finish({
              kind: persisted.kind === "ack" ? "acked" : "removed",
            });
            deletion.onerror = () => abort(deletion.error ?? new Error("IndexedDB terminal row delete failed"));
            return;
          }
          if (persisted.expected.phase === "resume-replay") {
            const claim: OutboundClaim = {
              ...persisted.expected,
              phase: "fresh-fallback",
              leaseUntil: Date.now() + OUTBOUND_CLAIM_LEASE_MS,
            };
            const update = store.put({ ...row, claim } satisfies DurableOutboundRow);
            update.onsuccess = () => finish({ kind: "fallback", claim });
            update.onerror = () => abort(update.error ?? new Error("IndexedDB fallback transition failed"));
            return;
          }
          const { claim: _claim, ...ready } = row;
          const update = store.put(ready satisfies DurableOutboundRow);
          update.onsuccess = () => finish({ kind: "released" });
          update.onerror = () => abort(update.error ?? new Error("IndexedDB terminal release failed"));
        }, abort);
      }, abort);
    });
  }

  claimOwner(hint: OutboundOwnerHint): Promise<DurableOutcome<OutboundOwnerContext>> {
    return writeTransaction((_store, _terminals, owners, settle, abort) => {
      const now = Date.now();
      readOwner(owners.get(hint.ownerId), (existing) => {
        if (!existing || existing.leaseUntil <= now) {
          const owner: DurableOutboundOwner = {
            key: hint.ownerId,
            ownerId: hint.ownerId,
            ownerInstanceId: hint.ownerInstanceId,
            ownerGeneration: (existing?.ownerGeneration ?? -1) + 1,
            leaseUntil: now + OUTBOUND_CLAIM_LEASE_MS,
          };
          putOwner(owners, owner, settle, abort);
          return;
        }
        if (existing.ownerInstanceId === hint.ownerInstanceId) {
          const owner = { ...existing, leaseUntil: now + OUTBOUND_CLAIM_LEASE_MS };
          putOwner(owners, owner, settle, abort);
          return;
        }
        if (
          hint.handoffToken
          && existing.handoff?.token === hint.handoffToken
          && existing.handoff.expiresAt > now
        ) {
          const owner: DurableOutboundOwner = {
            key: existing.key,
            ownerId: existing.ownerId,
            ownerInstanceId: hint.ownerInstanceId,
            ownerGeneration: existing.ownerGeneration + 1,
            leaseUntil: now + OUTBOUND_CLAIM_LEASE_MS,
            predecessor: {
              ownerInstanceId: existing.ownerInstanceId,
              ownerGeneration: existing.ownerGeneration,
              expiresAt: existing.handoff.expiresAt,
            },
          };
          putOwner(owners, owner, settle, abort);
          return;
        }
        const rotatedId = crypto.randomUUID();
        const owner: DurableOutboundOwner = {
          key: rotatedId,
          ownerId: rotatedId,
          ownerInstanceId: hint.ownerInstanceId,
          ownerGeneration: 0,
          leaseUntil: now + OUTBOUND_CLAIM_LEASE_MS,
        };
        putOwner(owners, owner, settle, abort);
      }, abort);
    });
  }

  renewOwner(owner: OutboundOwnerContext, leaseUntil: number): Promise<DurableOutcome<boolean>> {
    return this.mutateOwner(owner, (persisted) => ({ ...persisted, leaseUntil }));
  }

  prepareOwnerHandoff(
    owner: OutboundOwnerContext,
    handoff: OutboundOwnerHandoff,
  ): Promise<DurableOutcome<boolean>> {
    return this.mutateOwner(owner, (persisted) => ({ ...persisted, handoff }));
  }

  cancelOwnerHandoff(owner: OutboundOwnerContext): Promise<DurableOutcome<boolean>> {
    return this.mutateOwner(owner, (persisted) => {
      const { handoff: _handoff, ...withoutHandoff } = persisted;
      return withoutHandoff;
    });
  }

  private mutateClaim(
    accountKey: string,
    messageId: string,
    expected: OutboundClaim,
    mutate: (claim: OutboundClaim) => OutboundClaim,
  ): Promise<DurableOutcome<OutboundClaim | null>> {
    const key = rowKey(accountKey, messageId);
    return writeTransaction((store, _terminals, _owners, settle, abort) => {
      readRow(store.get(key), (row) => {
        if (!row || !sameClaim(row.claim, expected)) {
          settle(null);
          return;
        }
        const claim = mutate(row.claim!);
        const request = store.put({ ...row, claim } satisfies DurableOutboundRow);
        request.onsuccess = () => settle(claim);
        request.onerror = () => abort(request.error ?? new Error("IndexedDB claim update failed"));
      }, abort);
    });
  }

  private mutateOwner(
    owner: OutboundOwnerContext,
    mutate: (owner: DurableOutboundOwner) => DurableOutboundOwner,
  ): Promise<DurableOutcome<boolean>> {
    return writeTransaction((_store, _terminals, owners, settle, abort) => {
      readOwner(owners.get(owner.ownerId), (persisted) => {
        if (
          !persisted
          || persisted.ownerInstanceId !== owner.ownerInstanceId
          || persisted.ownerGeneration !== owner.ownerGeneration
        ) {
          settle(false);
          return;
        }
        const request = owners.put(mutate(persisted));
        request.onsuccess = () => settle(true);
        request.onerror = () => abort(request.error ?? new Error("IndexedDB owner update failed"));
      }, abort);
    });
  }
}

export function committedOrThrow<T>(operation: string, outcome: DurableOutcome<T>): T {
  if (outcome.kind === "committed") return outcome.value;
  throw new OutboundPersistenceError(operation, outcome.reason, outcome.cause);
}

export function createOutboundClaim(
  owner: OutboundOwnerContext,
  connectionGeneration: number,
  phase: OutboundClaimPhase,
): OutboundClaimRequest {
  return {
    ...owner,
    connectionGeneration,
    claimId: crypto.randomUUID(),
    phase,
    leaseUntil: Date.now() + OUTBOUND_CLAIM_LEASE_MS,
  };
}

/** Deterministic test adapter. Production callers use IndexedDB. */
export class MemoryDurableOutboundStore implements DurableOutboundStore {
  private readonly rows = new Map<string, DurableOutboundRow>();
  private readonly terminals = new Map<string, OutboundTerminalIntent>();
  private readonly owners = new Map<string, DurableOutboundOwner>();

  async list(accountKey: string): Promise<DurableOutcome<PersistedQueuedMessage[]>> {
    return {
      kind: "committed",
      value: [...this.rows.values()]
        .filter((row) => row.accountKey === accountKey)
        .map((row) => row.message),
    };
  }

  async scanAndPrune(accountKey: string, cutoff: number): Promise<DurableOutcome<DurableOutboundScan>> {
    const messages: PersistedQueuedMessage[] = [];
    const prunedIds: string[] = [];
    const now = Date.now();
    for (const [key, row] of this.rows) {
      if (row.accountKey !== accountKey) continue;
      const createdAt = Date.parse(row.message.createdAt);
      const stale = Number.isFinite(createdAt) && createdAt < cutoff;
      const liveClaim = !!row.claim && row.claim.leaseUntil > now;
      if (stale && !liveClaim && !this.terminals.has(key)) {
        this.rows.delete(key);
        prunedIds.push(row.message.id);
      } else {
        messages.push(row.message);
      }
    }
    return { kind: "committed", value: { messages, prunedIds } };
  }

  async persistReady(accountKey: string, message: PersistedQueuedMessage): Promise<DurableOutcome<void>> {
    const key = rowKey(accountKey, message.id);
    const existing = this.rows.get(key);
    if (existing?.claim && existing.claim.leaseUntil > Date.now()) {
      return failed(new DOMException("Message is claimed", "AbortError"));
    }
    if (this.terminals.has(key)) {
      return failed(new DOMException("Message has a pending terminal mutation", "AbortError"));
    }
    this.rows.set(key, { key, accountKey, incarnation: crypto.randomUUID(), message });
    return { kind: "committed", value: undefined };
  }

  async persistClaimed(
    accountKey: string,
    message: PersistedQueuedMessage,
    requestClaim: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundClaim>> {
    const key = rowKey(accountKey, message.id);
    const existing = this.rows.get(key);
    if (this.terminals.has(key)) {
      return failed(new DOMException("Message has a pending terminal mutation", "AbortError"));
    }
    if (existing?.claim && existing.claim.leaseUntil > Date.now()) {
      return failed(new DOMException("Message is claimed", "AbortError"));
    }
    const incarnation = crypto.randomUUID();
    const claim = claimForRow(requestClaim, incarnation);
    this.rows.set(key, { key, accountKey, incarnation, message, claim });
    return { kind: "committed", value: claim };
  }

  async claim(
    accountKey: string,
    messageId: string,
    requestClaim: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundClaimResult>> {
    const key = rowKey(accountKey, messageId);
    if (this.terminals.has(key)) return { kind: "committed", value: { kind: "terminal" } };
    const row = this.rows.get(key);
    if (!row) return { kind: "committed", value: { kind: "missing" } };
    if (row.claim && row.claim.leaseUntil > Date.now()) {
      return { kind: "committed", value: { kind: "busy", claim: row.claim } };
    }
    const claim = claimForRow(requestClaim, row.incarnation);
    this.rows.set(key, { ...row, claim });
    return { kind: "committed", value: { kind: "claimed", claim } };
  }

  async adopt(
    accountKey: string,
    messageId: string,
    requestClaim: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundClaimResult>> {
    const key = rowKey(accountKey, messageId);
    if (this.terminals.has(key)) return { kind: "committed", value: { kind: "terminal" } };
    const row = this.rows.get(key);
    if (!row) return { kind: "committed", value: { kind: "missing" } };
    const now = Date.now();
    if (row.claim && row.claim.leaseUntil > now) {
      const owner = this.owners.get(requestClaim.ownerId);
      const predecessor = owner?.predecessor;
      const authorized = !!owner
        && owner.ownerInstanceId === requestClaim.ownerInstanceId
        && owner.ownerGeneration === requestClaim.ownerGeneration
        && owner.leaseUntil > now
        && !!predecessor
        && predecessor.expiresAt > now
        && row.claim.ownerId === owner.ownerId
        && row.claim.ownerInstanceId === predecessor.ownerInstanceId
        && row.claim.ownerGeneration === predecessor.ownerGeneration;
      if (!authorized) {
        return { kind: "committed", value: { kind: "busy", claim: row.claim } };
      }
    }
    const claim = claimForRow(requestClaim, row.incarnation);
    this.rows.set(key, { ...row, claim });
    return { kind: "committed", value: { kind: "claimed", claim } };
  }

  async renew(accountKey: string, messageId: string, expected: OutboundClaim, leaseUntil: number): Promise<DurableOutcome<OutboundClaim | null>> {
    return this.mutate(accountKey, messageId, expected, (claim) => ({ ...claim, leaseUntil }));
  }

  async transition(accountKey: string, messageId: string, expected: OutboundClaim, phase: OutboundClaimPhase): Promise<DurableOutcome<OutboundClaim | null>> {
    return this.mutate(accountKey, messageId, expected, (claim) => ({
      ...claim,
      phase,
      leaseUntil: Date.now() + OUTBOUND_CLAIM_LEASE_MS,
    }));
  }

  async release(accountKey: string, messageId: string, expected: OutboundClaim): Promise<DurableOutcome<boolean>> {
    const key = rowKey(accountKey, messageId);
    const row = this.rows.get(key);
    if (!row || !sameClaim(row.claim, expected)) return { kind: "committed", value: false };
    const { claim: _claim, ...ready } = row;
    this.rows.set(key, ready);
    return { kind: "committed", value: true };
  }

  async deleteOwned(
    accountKey: string,
    messageId: string,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<boolean>> {
    const key = rowKey(accountKey, messageId);
    const row = this.rows.get(key);
    if (!row || !sameClaim(row.claim, expected)) return { kind: "committed", value: false };
    return { kind: "committed", value: this.rows.delete(key) };
  }

  async listTerminal(accountKey: string): Promise<DurableOutcome<OutboundTerminalIntent[]>> {
    return {
      kind: "committed",
      value: [...this.terminals.values()].filter((intent) => intent.accountKey === accountKey),
    };
  }

  async recordTerminal(
    accountKey: string,
    messageId: string,
    kind: OutboundTerminalKind,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<OutboundTerminalRecordResult>> {
    const key = terminalIntentKey(accountKey, messageId);
    const row = this.rows.get(key);
    if (!row) return { kind: "committed", value: { kind: "missing" } };
    if (!sameClaim(row.claim, expected) || row.incarnation !== expected.rowIncarnation) {
      return { kind: "committed", value: { kind: "stale" } };
    }
    const existing = this.terminals.get(key);
    if (existing) return { kind: "committed", value: { kind: "recorded", intent: existing } };
    const intent: OutboundTerminalIntent = {
      key,
      accountKey,
      messageId,
      kind,
      expected,
      recordedAt: Date.now(),
    };
    this.terminals.set(key, intent);
    return { kind: "committed", value: { kind: "recorded", intent } };
  }

  async applyTerminal(intent: OutboundTerminalIntent): Promise<DurableOutcome<OutboundTerminalApplyResult>> {
    const persisted = this.terminals.get(intent.key);
    if (!persisted) return { kind: "committed", value: { kind: "missing" } };
    const row = this.rows.get(intent.key);
    if (!row) {
      this.terminals.delete(intent.key);
      return { kind: "committed", value: { kind: "missing" } };
    }
    if (!sameClaim(row.claim, persisted.expected) || row.incarnation !== persisted.expected.rowIncarnation) {
      this.terminals.delete(intent.key);
      return { kind: "committed", value: { kind: "stale" } };
    }
    if (persisted.kind === "ack" || persisted.kind === "nonretryable") {
      this.rows.delete(intent.key);
      this.terminals.delete(intent.key);
      return {
        kind: "committed",
        value: { kind: persisted.kind === "ack" ? "acked" : "removed" },
      };
    }
    if (persisted.expected.phase === "resume-replay") {
      const claim: OutboundClaim = {
        ...persisted.expected,
        phase: "fresh-fallback",
        leaseUntil: Date.now() + OUTBOUND_CLAIM_LEASE_MS,
      };
      this.rows.set(intent.key, { ...row, claim });
      this.terminals.delete(intent.key);
      return { kind: "committed", value: { kind: "fallback", claim } };
    }
    const { claim: _claim, ...ready } = row;
    this.rows.set(intent.key, ready);
    this.terminals.delete(intent.key);
    return { kind: "committed", value: { kind: "released" } };
  }

  async claimOwner(hint: OutboundOwnerHint): Promise<DurableOutcome<OutboundOwnerContext>> {
    const now = Date.now();
    const existing = this.owners.get(hint.ownerId);
    let owner: DurableOutboundOwner;
    if (!existing || existing.leaseUntil <= now) {
      owner = {
        key: hint.ownerId,
        ownerId: hint.ownerId,
        ownerInstanceId: hint.ownerInstanceId,
        ownerGeneration: (existing?.ownerGeneration ?? -1) + 1,
        leaseUntil: now + OUTBOUND_CLAIM_LEASE_MS,
      };
    } else if (existing.ownerInstanceId === hint.ownerInstanceId) {
      owner = { ...existing, leaseUntil: now + OUTBOUND_CLAIM_LEASE_MS };
    } else if (
      hint.handoffToken
      && existing.handoff?.token === hint.handoffToken
      && existing.handoff.expiresAt > now
    ) {
      owner = {
        key: existing.key,
        ownerId: existing.ownerId,
        ownerInstanceId: hint.ownerInstanceId,
        ownerGeneration: existing.ownerGeneration + 1,
        leaseUntil: now + OUTBOUND_CLAIM_LEASE_MS,
        predecessor: {
          ownerInstanceId: existing.ownerInstanceId,
          ownerGeneration: existing.ownerGeneration,
          expiresAt: existing.handoff.expiresAt,
        },
      };
    } else {
      const rotatedId = crypto.randomUUID();
      owner = {
        key: rotatedId,
        ownerId: rotatedId,
        ownerInstanceId: hint.ownerInstanceId,
        ownerGeneration: 0,
        leaseUntil: now + OUTBOUND_CLAIM_LEASE_MS,
      };
    }
    this.owners.set(owner.ownerId, owner);
    return { kind: "committed", value: ownerContext(owner) };
  }

  async renewOwner(owner: OutboundOwnerContext, leaseUntil: number): Promise<DurableOutcome<boolean>> {
    return this.updateOwner(owner, (persisted) => ({ ...persisted, leaseUntil }));
  }

  async prepareOwnerHandoff(
    owner: OutboundOwnerContext,
    handoff: OutboundOwnerHandoff,
  ): Promise<DurableOutcome<boolean>> {
    return this.updateOwner(owner, (persisted) => ({ ...persisted, handoff }));
  }

  async cancelOwnerHandoff(owner: OutboundOwnerContext): Promise<DurableOutcome<boolean>> {
    return this.updateOwner(owner, (persisted) => {
      const { handoff: _handoff, ...withoutHandoff } = persisted;
      return withoutHandoff;
    });
  }

  private async mutate(
    accountKey: string,
    messageId: string,
    expected: OutboundClaim,
    update: (claim: OutboundClaim) => OutboundClaim,
  ): Promise<DurableOutcome<OutboundClaim | null>> {
    const key = rowKey(accountKey, messageId);
    const row = this.rows.get(key);
    if (!row || !sameClaim(row.claim, expected)) return { kind: "committed", value: null };
    const claim = update(row.claim!);
    this.rows.set(key, { ...row, claim });
    return { kind: "committed", value: claim };
  }

  private async updateOwner(
    owner: OutboundOwnerContext,
    update: (owner: DurableOutboundOwner) => DurableOutboundOwner,
  ): Promise<DurableOutcome<boolean>> {
    const persisted = this.owners.get(owner.ownerId);
    if (
      !persisted
      || persisted.ownerInstanceId !== owner.ownerInstanceId
      || persisted.ownerGeneration !== owner.ownerGeneration
    ) {
      return { kind: "committed", value: false };
    }
    this.owners.set(owner.ownerId, update(persisted));
    return { kind: "committed", value: true };
  }
}
