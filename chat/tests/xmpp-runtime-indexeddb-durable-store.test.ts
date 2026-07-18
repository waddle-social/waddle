import { describe, expect, test } from "bun:test";
import { IDBFactory } from "fake-indexeddb";
import type { PersistedQueuedDmMessage } from "../src/lib/outbound-queue-store";
import { committedOrThrow } from "../src/lib/xmpp-runtime/durable-contract";
import { IndexedDbDurableOutboundStore } from "../src/lib/xmpp-runtime/indexeddb-durable-store";

const ACCOUNT = "indexeddb-repository@example.com";
const CLOCK = { now: () => 1_000 };

function directMessage(id: string): PersistedQueuedDmMessage {
  return {
    kind: "dm",
    id,
    createdAt: "2026-07-18T00:00:00.000Z",
    peerJid: "recipient@example.com",
    body: "hello",
  };
}

function openRawDatabase(
  indexedDb: IDBFactory,
  name: string,
): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDb.open(name);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

function transactionDone(transaction: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    transaction.oncomplete = () => resolve();
    transaction.onabort = () => reject(transaction.error);
    transaction.onerror = () => {
      // `onabort` owns the terminal result.
    };
  });
}

async function countRawAccounts(
  indexedDb: IDBFactory,
  databaseName: string,
): Promise<number> {
  const database = await openRawDatabase(indexedDb, databaseName);
  const transaction = database.transaction("accounts", "readonly");
  const request = transaction.objectStore("accounts").count();
  const count = await new Promise<number>((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
  await transactionDone(transaction);
  database.close();
  return count;
}

function instrumentOpenedDatabase(
  indexedDb: IDBFactory,
  instrument: (database: IDBDatabase) => void,
): void {
  const open = indexedDb.open.bind(indexedDb);
  Object.defineProperty(indexedDb, "open", {
    configurable: true,
    value: (name: string, version?: number) => {
      const request = version === undefined
        ? open(name)
        : open(name, version);
      request.addEventListener("success", () => instrument(request.result), {
        once: true,
      });
      return request;
    },
  });
}

function instrumentFirstRepositoryTransaction(
  indexedDb: IDBFactory,
  instrument: (
    transaction: IDBTransaction,
    options: IDBTransactionOptions | undefined,
  ) => void,
): void {
  instrumentOpenedDatabase(indexedDb, (database) => {
    const transaction = database.transaction.bind(database);
    let instrumented = false;
    Object.defineProperty(database, "transaction", {
      configurable: true,
      value: (
        storeNames: string | string[],
        mode?: IDBTransactionMode,
        options?: IDBTransactionOptions,
      ) => {
        const created = transaction(storeNames, mode, options);
        if (!instrumented) {
          instrumented = true;
          instrument(created, options);
        }
        return created;
      },
    });
  });
}

function patchObjectStore(
  transaction: IDBTransaction,
  patch: (store: IDBObjectStore) => void,
): void {
  const objectStore = transaction.objectStore.bind(transaction);
  Object.defineProperty(transaction, "objectStore", {
    configurable: true,
    value: (name: string) => {
      const store = objectStore(name);
      patch(store);
      return store;
    },
  });
}

describe("IndexedDB durable repository seam", () => {
  test("uses strict durability, synchronous put, and resolves only after completion", async () => {
    const indexedDb = new IDBFactory();
    const databaseName = `strict-${crypto.randomUUID()}`;
    let phase = "before";
    let putCalls = 0;
    let transactionCompleted = false;
    let observedDurability: IDBTransactionDurability | undefined;
    instrumentFirstRepositoryTransaction(
      indexedDb,
      (transaction, options) => {
        observedDurability = options?.durability;
        transaction.addEventListener("complete", () => {
          transactionCompleted = true;
        });
        patchObjectStore(transaction, (store) => {
          const get = store.get.bind(store);
          Object.defineProperty(store, "get", {
            configurable: true,
            value: (query: IDBValidKey | IDBKeyRange) => {
              const request = get(query);
              request.addEventListener("success", () => {
                phase = "get-success";
                queueMicrotask(() => {
                  phase = "microtask";
                });
              }, { once: true });
              return request;
            },
          });
          const put = store.put.bind(store);
          Object.defineProperty(store, "put", {
            configurable: true,
            value: (value: unknown) => {
              putCalls += 1;
              expect(phase).toBe("get-success");
              return put(value);
            },
          });
        });
      },
    );
    const store = new IndexedDbDurableOutboundStore({
      authorityClock: CLOCK,
      indexedDb,
      databaseName,
    });

    const value = committedOrThrow(
      "strict-persist",
      await store.persistReady(ACCOUNT, directMessage("strict")),
    );

    expect(value.kind).toBe("inserted");
    expect(observedDurability).toBe("strict");
    expect(putCalls).toBe(1);
    expect(transactionCompleted).toBe(true);
    await store.close();
  });

  test("preserves the first read-boundary exception and writes nothing", async () => {
    const indexedDb = new IDBFactory();
    const databaseName = `read-error-${crypto.randomUUID()}`;
    const marker = new Error("get failed");
    instrumentFirstRepositoryTransaction(indexedDb, (transaction) => {
      patchObjectStore(transaction, (store) => {
        Object.defineProperty(store, "get", {
          configurable: true,
          value: () => {
            throw marker;
          },
        });
      });
    });
    const store = new IndexedDbDurableOutboundStore({
      authorityClock: CLOCK,
      indexedDb,
      databaseName,
    });

    expect(await store.revision(ACCOUNT)).toEqual({
      kind: "failed",
      reason: "unavailable",
      cause: marker,
    });
    await store.close();
    expect(await countRawAccounts(indexedDb, databaseName)).toBe(0);
  });

  test("preserves the first put exception and writes nothing", async () => {
    const indexedDb = new IDBFactory();
    const databaseName = `put-error-${crypto.randomUUID()}`;
    const marker = new Error("put failed");
    instrumentFirstRepositoryTransaction(indexedDb, (transaction) => {
      patchObjectStore(transaction, (store) => {
        Object.defineProperty(store, "put", {
          configurable: true,
          value: () => {
            throw marker;
          },
        });
      });
    });
    const store = new IndexedDbDurableOutboundStore({
      authorityClock: CLOCK,
      indexedDb,
      databaseName,
    });

    expect(await store.persistReady(
      ACCOUNT,
      directMessage("put-error"),
    )).toEqual({
      kind: "failed",
      reason: "unavailable",
      cause: marker,
    });
    await store.close();
    expect(await countRawAccounts(indexedDb, databaseName)).toBe(0);
  });

  test("fails promptly when a transaction completes without a value", async () => {
    const indexedDb = new IDBFactory();
    const databaseName = `terminal-${crypto.randomUUID()}`;
    let putCalls = 0;
    instrumentFirstRepositoryTransaction(indexedDb, (transaction) => {
      patchObjectStore(transaction, (objectStore) => {
        Object.defineProperty(objectStore, "get", {
          configurable: true,
          value: () => ({ onsuccess: null, onerror: null }),
        });
        const put = objectStore.put.bind(objectStore);
        Object.defineProperty(objectStore, "put", {
          configurable: true,
          value: (value: unknown) => {
            putCalls += 1;
            return put(value);
          },
        });
      });
    });
    const store = new IndexedDbDurableOutboundStore({
      authorityClock: CLOCK,
      indexedDb,
      databaseName,
    });
    let timeout: ReturnType<typeof setTimeout> | undefined;

    const outcome = await Promise.race([
      store.revision(ACCOUNT),
      new Promise<never>((_resolve, reject) => {
        timeout = setTimeout(() => {
          reject(new Error("IndexedDB operation hung after completion"));
        }, 250);
      }),
    ]).finally(() => {
      if (timeout) clearTimeout(timeout);
    });

    expect(outcome.kind).toBe("failed");
    if (outcome.kind === "failed") {
      expect(outcome.reason).toBe("aborted");
      expect(outcome.cause).toBeInstanceOf(DOMException);
      expect(outcome.cause).toMatchObject({
        name: "AbortError",
        message: "IndexedDB operation did not settle",
      });
    }
    expect(putCalls).toBe(0);
    await store.close();
    expect(await countRawAccounts(indexedDb, databaseName)).toBe(0);
  });

  test("retries a failed open and repeated close forces a fresh connection", async () => {
    const indexedDb = new IDBFactory();
    const open = indexedDb.open.bind(indexedDb);
    const marker = new Error("open failed");
    let openCalls = 0;
    Object.defineProperty(indexedDb, "open", {
      configurable: true,
      value: (name: string, version?: number) => {
        openCalls += 1;
        if (openCalls === 1) throw marker;
        return version === undefined
          ? open(name)
          : open(name, version);
      },
    });
    const store = new IndexedDbDurableOutboundStore({
      authorityClock: CLOCK,
      indexedDb,
      databaseName: `retry-${crypto.randomUUID()}`,
    });

    expect(await store.revision(ACCOUNT)).toEqual({
      kind: "failed",
      reason: "unavailable",
      cause: marker,
    });
    expect(committedOrThrow(
      "retry-open",
      await store.revision(ACCOUNT),
    )).toBe(0);
    expect(openCalls).toBe(2);

    await store.close();
    await store.close();
    expect(committedOrThrow(
      "after-repeated-close",
      await store.revision(ACCOUNT),
    )).toBe(0);
    expect(openCalls).toBe(3);
    await store.close();
  });

  test("rejects invalid database versions before opening", () => {
    const indexedDb = new IDBFactory();

    for (const databaseVersion of [
      0,
      -1,
      1.5,
      Number.NaN,
      Number.POSITIVE_INFINITY,
    ]) {
      expect(() => new IndexedDbDurableOutboundStore({
        indexedDb,
        databaseVersion,
      })).toThrow(RangeError);
    }
  });
});
