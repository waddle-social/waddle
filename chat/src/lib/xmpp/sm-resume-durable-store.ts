import type { DurableOutcome } from "../outbound-durable-store";
import type { PersistedSmResumeState } from "./resume-persistence";

const DATABASE_NAME = "waddle-chat-sm-resume";
const DATABASE_VERSION = 1;
const STORE_NAME = "snapshots";

export type DurableSmEnvelope = {
  accountKey: string;
  version: number;
  state: PersistedSmResumeState;
  savedAt: number;
  ownerId: string;
  consumed: boolean;
};

export interface DurableSmResumeStore {
  load(accountKey: string): Promise<DurableOutcome<DurableSmEnvelope | null>>;
  consume(
    accountKey: string,
    ownerId: string,
    usable: (envelope: DurableSmEnvelope) => boolean,
  ): Promise<DurableOutcome<DurableSmEnvelope | null>>;
  save(
    accountKey: string,
    ownerId: string,
    state: PersistedSmResumeState,
    savedAt: number,
  ): Promise<DurableOutcome<DurableSmEnvelope>>;
  clear(accountKey: string): Promise<DurableOutcome<boolean>>;
}

function classifyFailure(cause: unknown): "unavailable" | "quota" | "security" | "aborted" {
  const name = cause instanceof Error ? cause.name : "";
  if (name === "QuotaExceededError") return "quota";
  if (name === "SecurityError") return "security";
  if (name === "AbortError" || name === "TransactionInactiveError") return "aborted";
  return "unavailable";
}

function failed<T>(cause: unknown): DurableOutcome<T> {
  return { kind: "failed", reason: classifyFailure(cause), cause };
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
        request.result.createObjectStore(STORE_NAME, { keyPath: "accountKey" });
      }
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error("SM IndexedDB open failed"));
    request.onblocked = () => reject(new DOMException("SM IndexedDB upgrade blocked", "AbortError"));
  }).catch((error) => {
    databasePromise = null;
    throw error;
  });
  databasePromise = opening;
  return opening;
}

async function transaction<T>(
  mode: IDBTransactionMode,
  action: (
    store: IDBObjectStore,
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
    let settled = false;
    let operationError: unknown;
    let tx: IDBTransaction;
    try {
      tx = database.transaction(STORE_NAME, mode, { durability: "strict" });
      action(
        tx.objectStore(STORE_NAME),
        (value) => {
          result = value;
          settled = true;
        },
        (cause) => {
          operationError = cause;
          tx.abort();
        },
      );
    } catch (error) {
      resolve(failed(error));
      return;
    }
    tx.oncomplete = () => {
      if (!settled) {
        resolve(failed(new DOMException("SM transaction did not settle", "AbortError")));
        return;
      }
      resolve({ kind: "committed", value: result as T });
    };
    tx.onerror = () => {
      // `onabort` owns completion.
    };
    tx.onabort = () => resolve(failed(operationError ?? tx.error));
  });
}

function read(
  request: IDBRequest,
  success: (value: DurableSmEnvelope | undefined) => void,
  abort: (cause: unknown) => void,
): void {
  request.onsuccess = () => success(request.result as DurableSmEnvelope | undefined);
  request.onerror = () => abort(request.error ?? new Error("SM IndexedDB read failed"));
}

export class IndexedDbDurableSmResumeStore implements DurableSmResumeStore {
  load(accountKey: string): Promise<DurableOutcome<DurableSmEnvelope | null>> {
    return transaction("readonly", (store, settle, abort) => {
      read(store.get(accountKey), (value) => settle(value ?? null), abort);
    });
  }

  consume(
    accountKey: string,
    ownerId: string,
    usable: (envelope: DurableSmEnvelope) => boolean,
  ): Promise<DurableOutcome<DurableSmEnvelope | null>> {
    return transaction("readwrite", (store, settle, abort) => {
      read(store.get(accountKey), (envelope) => {
        if (!envelope || envelope.ownerId !== ownerId || envelope.consumed || !usable(envelope)) {
          settle(null);
          return;
        }
        const consumed = { ...envelope, consumed: true } satisfies DurableSmEnvelope;
        const request = store.put(consumed);
        request.onsuccess = () => settle(consumed);
        request.onerror = () => abort(request.error ?? new Error("SM consume update failed"));
      }, abort);
    });
  }

  save(
    accountKey: string,
    ownerId: string,
    state: PersistedSmResumeState,
    savedAt: number,
  ): Promise<DurableOutcome<DurableSmEnvelope>> {
    return transaction("readwrite", (store, settle, abort) => {
      read(store.get(accountKey), (previous) => {
        const envelope: DurableSmEnvelope = {
          accountKey,
          version: (previous?.version ?? 0) + 1,
          state,
          savedAt,
          ownerId,
          consumed: false,
        };
        const request = store.put(envelope);
        request.onsuccess = () => settle(envelope);
        request.onerror = () => abort(request.error ?? new Error("SM snapshot write failed"));
      }, abort);
    });
  }

  clear(accountKey: string): Promise<DurableOutcome<boolean>> {
    return transaction("readwrite", (store, settle, abort) => {
      read(store.get(accountKey), (existing) => {
        if (!existing) {
          settle(false);
          return;
        }
        const request = store.delete(accountKey);
        request.onsuccess = () => settle(true);
        request.onerror = () => abort(request.error ?? new Error("SM snapshot delete failed"));
      }, abort);
    });
  }
}

export class MemoryDurableSmResumeStore implements DurableSmResumeStore {
  private readonly envelopes = new Map<string, DurableSmEnvelope>();

  async load(accountKey: string): Promise<DurableOutcome<DurableSmEnvelope | null>> {
    return { kind: "committed", value: this.envelopes.get(accountKey) ?? null };
  }

  async consume(
    accountKey: string,
    ownerId: string,
    usable: (envelope: DurableSmEnvelope) => boolean,
  ): Promise<DurableOutcome<DurableSmEnvelope | null>> {
    const envelope = this.envelopes.get(accountKey);
    if (!envelope || envelope.ownerId !== ownerId || envelope.consumed || !usable(envelope)) {
      return { kind: "committed", value: null };
    }
    const consumed = { ...envelope, consumed: true };
    this.envelopes.set(accountKey, consumed);
    return { kind: "committed", value: consumed };
  }

  async save(
    accountKey: string,
    ownerId: string,
    state: PersistedSmResumeState,
    savedAt: number,
  ): Promise<DurableOutcome<DurableSmEnvelope>> {
    const envelope: DurableSmEnvelope = {
      accountKey,
      version: (this.envelopes.get(accountKey)?.version ?? 0) + 1,
      state,
      savedAt,
      ownerId,
      consumed: false,
    };
    this.envelopes.set(accountKey, envelope);
    return { kind: "committed", value: envelope };
  }

  async clear(accountKey: string): Promise<DurableOutcome<boolean>> {
    return { kind: "committed", value: this.envelopes.delete(accountKey) };
  }
}
