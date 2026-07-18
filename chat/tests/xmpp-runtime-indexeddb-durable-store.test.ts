import { describe, expect, test } from "bun:test";
import { IDBFactory } from "fake-indexeddb";
import { committedOrThrow } from "../src/lib/xmpp-runtime/durable-contract";
import { emptyAccount } from "../src/lib/xmpp-runtime/durable-model";
import {
  IndexedDbDurableAccountRepository,
  IndexedDbDurableOutboundStore,
} from "../src/lib/xmpp-runtime/indexeddb-durable-store";

const ACCOUNT = "indexeddb-repository@example.com";
const CLOCK = { now: () => 1_000 };

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
          const put = store.put.bind(store);
          Object.defineProperty(store, "put", {
            configurable: true,
            value: (value: unknown) => {
              putCalls += 1;
              expect(phase).toBe("callback-returned");
              return put(value);
            },
          });
        });
      },
    );
    const repository = new IndexedDbDurableAccountRepository({
      indexedDb,
      databaseName,
    });

    const value = await repository.transact(ACCOUNT, () => {
      phase = "callback-returned";
      queueMicrotask(() => {
        phase = "microtask";
      });
      return {
        account: emptyAccount(ACCOUNT),
        write: true,
        value: { committed: true },
      };
    });

    expect(value).toEqual({ committed: true });
    expect(observedDurability).toBe("strict");
    expect(putCalls).toBe(1);
    expect(transactionCompleted).toBe(true);
    await repository.close();
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
    const repository = new IndexedDbDurableAccountRepository({
      indexedDb,
      databaseName,
    });

    await expect(repository.transact(ACCOUNT, () => ({
      account: emptyAccount(ACCOUNT),
      write: true,
      value: "unreachable",
    }))).rejects.toBe(marker);
    await repository.close();
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
    const repository = new IndexedDbDurableAccountRepository({
      indexedDb,
      databaseName,
    });

    await expect(repository.transact(ACCOUNT, () => ({
      account: emptyAccount(ACCOUNT),
      write: true,
      value: "uncommitted",
    }))).rejects.toBe(marker);
    await repository.close();
    expect(await countRawAccounts(indexedDb, databaseName)).toBe(0);
  });

  test("aborts clone and account-key failures with no durable write", async () => {
    const indexedDb = new IDBFactory();
    const databaseName = `validation-${crypto.randomUUID()}`;
    const repository = new IndexedDbDurableAccountRepository({
      indexedDb,
      databaseName,
    });

    await expect(repository.transact(ACCOUNT, () => ({
      account: emptyAccount(ACCOUNT),
      write: true,
      value: { uncloneable: () => undefined },
    }))).rejects.toMatchObject({ name: "DataCloneError" });
    await expect(repository.transact(ACCOUNT, () => ({
      account: emptyAccount("other@example.com"),
      write: true,
      value: "mismatched",
    }))).rejects.toMatchObject({ name: "DataError" });

    await repository.close();
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
