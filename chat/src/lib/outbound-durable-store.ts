import type { PersistedQueuedMessage } from "./outbound-queue-store";

const DATABASE_NAME = "waddle-chat-outbound";
const DATABASE_VERSION = 1;
const STORE_NAME = "messages";

export const OUTBOUND_CLAIM_LEASE_MS = 45_000;

export type OutboundClaimPhase =
  | "sending"
  | "resume-replay"
  | "fresh-fallback";

export type OutboundClaim = {
  ownerId: string;
  connectionGeneration: number;
  claimId: string;
  phase: OutboundClaimPhase;
  leaseUntil: number;
};

type DurableOutboundRow = {
  key: string;
  accountKey: string;
  message: PersistedQueuedMessage;
  claim?: OutboundClaim;
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
  persistReady(accountKey: string, message: PersistedQueuedMessage): Promise<DurableOutcome<void>>;
  persistClaimed(
    accountKey: string,
    message: PersistedQueuedMessage,
    claim: OutboundClaim,
  ): Promise<DurableOutcome<OutboundClaim>>;
  claim(
    accountKey: string,
    messageId: string,
    claim: OutboundClaim,
  ): Promise<DurableOutcome<OutboundClaim | null>>;
  adopt(
    accountKey: string,
    messageId: string,
    claim: OutboundClaim,
  ): Promise<DurableOutcome<OutboundClaim | null>>;
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
  delete(accountKey: string, messageId: string): Promise<DurableOutcome<boolean>>;
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
    && left.connectionGeneration === right.connectionGeneration
    && left.claimId === right.claimId;
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
      if (!request.result.objectStoreNames.contains(STORE_NAME)) {
        request.result.createObjectStore(STORE_NAME, { keyPath: "key" });
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
  mutate: (store: IDBObjectStore, settle: (value: T) => void, abort: (cause: unknown) => void) => void,
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
      transaction = database.transaction(STORE_NAME, "readwrite", { durability: "strict" });
      mutate(
        transaction.objectStore(STORE_NAME),
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

function readRow(request: IDBRequest, onSuccess: (row: DurableOutboundRow | undefined) => void, abort: (cause: unknown) => void): void {
  request.onsuccess = () => onSuccess(request.result as DurableOutboundRow | undefined);
  request.onerror = () => abort(request.error ?? new Error("IndexedDB read failed"));
}

export class IndexedDbDurableOutboundStore implements DurableOutboundStore {
  list(accountKey: string): Promise<DurableOutcome<PersistedQueuedMessage[]>> {
    return writeTransaction((store, settle, abort) => {
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

  persistReady(accountKey: string, message: PersistedQueuedMessage): Promise<DurableOutcome<void>> {
    const key = rowKey(accountKey, message.id);
    return writeTransaction((store, settle, abort) => {
      readRow(store.get(key), (existing) => {
        if (existing?.claim && existing.claim.leaseUntil > Date.now()) {
          abort(new DOMException("Message is claimed by another connection", "AbortError"));
          return;
        }
        const request = store.put({ key, accountKey, message } satisfies DurableOutboundRow);
        request.onsuccess = () => settle(undefined);
        request.onerror = () => abort(request.error ?? new Error("IndexedDB put failed"));
      }, abort);
    });
  }

  persistClaimed(
    accountKey: string,
    message: PersistedQueuedMessage,
    claim: OutboundClaim,
  ): Promise<DurableOutcome<OutboundClaim>> {
    const key = rowKey(accountKey, message.id);
    return writeTransaction((store, settle, abort) => {
      readRow(store.get(key), (existing) => {
        if (
          existing?.claim
          && existing.claim.leaseUntil > Date.now()
          && !sameClaim(existing.claim, claim)
        ) {
          abort(new DOMException("Message is claimed by another connection", "AbortError"));
          return;
        }
        const request = store.put({ key, accountKey, message, claim } satisfies DurableOutboundRow);
        request.onsuccess = () => settle(claim);
        request.onerror = () => abort(request.error ?? new Error("IndexedDB put failed"));
      }, abort);
    });
  }

  claim(
    accountKey: string,
    messageId: string,
    claim: OutboundClaim,
  ): Promise<DurableOutcome<OutboundClaim | null>> {
    const key = rowKey(accountKey, messageId);
    return writeTransaction((store, settle, abort) => {
      readRow(store.get(key), (row) => {
        if (!row) {
          settle(null);
          return;
        }
        if (row.claim && row.claim.leaseUntil > Date.now() && !sameClaim(row.claim, claim)) {
          settle(null);
          return;
        }
        const request = store.put({ ...row, claim } satisfies DurableOutboundRow);
        request.onsuccess = () => settle(claim);
        request.onerror = () => abort(request.error ?? new Error("IndexedDB claim failed"));
      }, abort);
    });
  }

  adopt(
    accountKey: string,
    messageId: string,
    claim: OutboundClaim,
  ): Promise<DurableOutcome<OutboundClaim | null>> {
    const key = rowKey(accountKey, messageId);
    return writeTransaction((store, settle, abort) => {
      readRow(store.get(key), (row) => {
        if (!row) {
          settle(null);
          return;
        }
        const request = store.put({ ...row, claim } satisfies DurableOutboundRow);
        request.onsuccess = () => settle(claim);
        request.onerror = () => abort(request.error ?? new Error("IndexedDB adoption failed"));
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
    return writeTransaction((store, settle, abort) => {
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

  delete(accountKey: string, messageId: string): Promise<DurableOutcome<boolean>> {
    const key = rowKey(accountKey, messageId);
    return writeTransaction((store, settle, abort) => {
      readRow(store.get(key), (row) => {
        if (!row) {
          settle(false);
          return;
        }
        const request = store.delete(key);
        request.onsuccess = () => settle(true);
        request.onerror = () => abort(request.error ?? new Error("IndexedDB delete failed"));
      }, abort);
    });
  }

  private mutateClaim(
    accountKey: string,
    messageId: string,
    expected: OutboundClaim,
    mutate: (claim: OutboundClaim) => OutboundClaim,
  ): Promise<DurableOutcome<OutboundClaim | null>> {
    const key = rowKey(accountKey, messageId);
    return writeTransaction((store, settle, abort) => {
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
}

export function committedOrThrow<T>(operation: string, outcome: DurableOutcome<T>): T {
  if (outcome.kind === "committed") return outcome.value;
  throw new OutboundPersistenceError(operation, outcome.reason, outcome.cause);
}

export function createOutboundClaim(
  ownerId: string,
  connectionGeneration: number,
  phase: OutboundClaimPhase,
): OutboundClaim {
  return {
    ownerId,
    connectionGeneration,
    claimId: crypto.randomUUID(),
    phase,
    leaseUntil: Date.now() + OUTBOUND_CLAIM_LEASE_MS,
  };
}

/** Deterministic test adapter. Production callers use IndexedDB. */
export class MemoryDurableOutboundStore implements DurableOutboundStore {
  private readonly rows = new Map<string, DurableOutboundRow>();

  async list(accountKey: string): Promise<DurableOutcome<PersistedQueuedMessage[]>> {
    return {
      kind: "committed",
      value: [...this.rows.values()]
        .filter((row) => row.accountKey === accountKey)
        .map((row) => row.message),
    };
  }

  async persistReady(accountKey: string, message: PersistedQueuedMessage): Promise<DurableOutcome<void>> {
    const key = rowKey(accountKey, message.id);
    const existing = this.rows.get(key);
    if (existing?.claim && existing.claim.leaseUntil > Date.now()) {
      return failed(new DOMException("Message is claimed", "AbortError"));
    }
    this.rows.set(key, { key, accountKey, message });
    return { kind: "committed", value: undefined };
  }

  async persistClaimed(
    accountKey: string,
    message: PersistedQueuedMessage,
    claim: OutboundClaim,
  ): Promise<DurableOutcome<OutboundClaim>> {
    const key = rowKey(accountKey, message.id);
    const existing = this.rows.get(key);
    if (existing?.claim && existing.claim.leaseUntil > Date.now() && !sameClaim(existing.claim, claim)) {
      return failed(new DOMException("Message is claimed", "AbortError"));
    }
    this.rows.set(key, { key, accountKey, message, claim });
    return { kind: "committed", value: claim };
  }

  async claim(accountKey: string, messageId: string, claim: OutboundClaim): Promise<DurableOutcome<OutboundClaim | null>> {
    const key = rowKey(accountKey, messageId);
    const row = this.rows.get(key);
    if (!row) return { kind: "committed", value: null };
    if (row.claim && row.claim.leaseUntil > Date.now() && !sameClaim(row.claim, claim)) {
      return { kind: "committed", value: null };
    }
    this.rows.set(key, { ...row, claim });
    return { kind: "committed", value: claim };
  }

  async adopt(accountKey: string, messageId: string, claim: OutboundClaim): Promise<DurableOutcome<OutboundClaim | null>> {
    const key = rowKey(accountKey, messageId);
    const row = this.rows.get(key);
    if (!row) return { kind: "committed", value: null };
    this.rows.set(key, { ...row, claim });
    return { kind: "committed", value: claim };
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

  async delete(accountKey: string, messageId: string): Promise<DurableOutcome<boolean>> {
    return { kind: "committed", value: this.rows.delete(rowKey(accountKey, messageId)) };
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
}
