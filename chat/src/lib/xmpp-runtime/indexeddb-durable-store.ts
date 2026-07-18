import {
  DurableStoreEngine,
  systemAuthorityClock,
  type DurableAccountCommit,
  type DurableAccountRepository,
  type DurableAccountTransaction,
} from "./durable-engine";
import type { DurableAuthorityClock } from "./durable-contract";

const DATABASE_NAME = "waddle-chat-xmpp-runtime-v1";
const DATABASE_VERSION = 1;
const ACCOUNT_STORE_NAME = "accounts";

export type IndexedDbDurableOutboundStoreOptions = {
  authorityClock?: DurableAuthorityClock;
  databaseName?: string;
  databaseVersion?: number;
  indexedDb?: IDBFactory;
};

function assertAccountKey<T>(
  accountKey: string,
  commit: DurableAccountCommit<T>,
): void {
  if (commit.account.accountKey !== accountKey) {
    throw new DOMException(
      "Durable repository account key mismatch",
      "DataError",
    );
  }
}

function openDatabase(
  indexedDb: IDBFactory | undefined,
  databaseName: string,
  databaseVersion: number,
  prepare: (database: IDBDatabase) => void,
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
        request.result.createObjectStore(ACCOUNT_STORE_NAME, {
          keyPath: "accountKey",
        });
      }
    };
    request.onsuccess = () => {
      if (settled) {
        request.result.close();
        return;
      }
      try {
        prepare(request.result);
        settled = true;
        resolve(request.result);
      } catch (error) {
        request.result.close();
        rejectOnce(error);
      }
    };
    request.onerror = () => rejectOnce(
      request.error ?? new Error("IndexedDB open failed"),
    );
    request.onblocked = () => rejectOnce(
      new DOMException("IndexedDB upgrade blocked", "AbortError"),
    );
  });
}

/** Repository boundary exported for direct adapter conformance tests. */
export class IndexedDbDurableAccountRepository
implements DurableAccountRepository {
  private readonly indexedDb: IDBFactory | undefined;
  private readonly databaseName: string;
  private readonly databaseVersion: number;
  private databasePromise: Promise<IDBDatabase> | null = null;
  private databaseConnection: IDBDatabase | null = null;

  constructor(options: IndexedDbDurableOutboundStoreOptions) {
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

  transact<T>(
    accountKey: string,
    run: DurableAccountTransaction<T>,
  ): Promise<T> {
    return this.runTransaction(accountKey, run);
  }

  async close(): Promise<void> {
    const opening = this.databasePromise;
    const connection = this.databaseConnection;
    if (this.databasePromise === opening) {
      this.databasePromise = null;
    }
    if (!opening && !connection) return;
    if (connection) {
      if (this.databaseConnection === connection) {
        this.databaseConnection = null;
      }
      connection.close();
      return;
    }
    if (!opening) return;
    try {
      const database = await opening;
      if (this.databaseConnection === database) {
        this.databaseConnection = null;
      }
      database.close();
    } catch {
      // A failed or blocked open has no connection to close.
    }
  }

  private database(): Promise<IDBDatabase> {
    if (this.databaseConnection) {
      return Promise.resolve(this.databaseConnection);
    }
    if (this.databasePromise) return this.databasePromise;
    let opening: Promise<IDBDatabase>;
    opening = openDatabase(
      this.indexedDb,
      this.databaseName,
      this.databaseVersion,
      (database) => {
        const clearCachedDatabase = (): void => {
          if (this.databaseConnection === database) {
            this.databaseConnection = null;
          }
          if (this.databasePromise === opening) {
            this.databasePromise = null;
          }
        };
        database.onversionchange = () => {
          database.close();
          clearCachedDatabase();
        };
        database.onclose = clearCachedDatabase;
      },
    );
    this.databasePromise = opening;
    void opening.then(
      (database) => {
        if (this.databasePromise !== opening) {
          database.close();
          return;
        }
        this.databaseConnection = database;
      },
      () => {
        if (this.databasePromise === opening) {
          this.databasePromise = null;
        }
      },
    );
    return opening;
  }

  private async runTransaction<T>(
    accountKey: string,
    run: DurableAccountTransaction<T>,
  ): Promise<T> {
    const database = await this.database();
    return new Promise<T>((resolve, reject) => {
      let value: T | undefined;
      let valueReady = false;
      let operationError: unknown;
      let operationErrorReady = false;
      let transaction: IDBTransaction;
      try {
        transaction = database.transaction(
          ACCOUNT_STORE_NAME,
          "readwrite",
          { durability: "strict" },
        );
      } catch (error) {
        reject(error);
        return;
      }

      const abortWith = (error: unknown): void => {
        if (!operationErrorReady) {
          operationError = error;
          operationErrorReady = true;
        }
        try {
          transaction.abort();
        } catch {
          // An already-aborting transaction still terminates through onabort.
        }
      };

      transaction.oncomplete = () => {
        if (valueReady) resolve(value as T);
      };
      transaction.onerror = () => {
        // `onabort` owns transaction-error rejection.
      };
      transaction.onabort = () => {
        reject(
          operationErrorReady
            ? operationError
            : transaction.error
              ?? new DOMException("IndexedDB transaction aborted", "AbortError"),
        );
      };

      try {
        const store = transaction.objectStore(ACCOUNT_STORE_NAME);
        const read = store.get(accountKey);
        read.onsuccess = () => {
          try {
            const commit = run(read.result);
            assertAccountKey(accountKey, commit);
            value = structuredClone(commit.value);
            valueReady = true;
            if (!commit.write) return;
            const write = store.put(structuredClone(commit.account));
            write.onerror = () => abortWith(
              write.error ?? new Error("IndexedDB account write failed"),
            );
          } catch (error) {
            abortWith(error);
          }
        };
        read.onerror = () => abortWith(
          read.error ?? new Error("IndexedDB account read failed"),
        );
      } catch (error) {
        abortWith(error);
      }
    });
  }
}

export class IndexedDbDurableOutboundStore extends DurableStoreEngine {
  constructor(options: IndexedDbDurableOutboundStoreOptions = {}) {
    const repository = new IndexedDbDurableAccountRepository(options);
    super(repository, options.authorityClock ?? systemAuthorityClock);
  }
}
