import { describe, expect, test } from "bun:test";
import { IDBFactory } from "fake-indexeddb";
import type {
  PersistedQueuedDmMessage,
  PersistedQueuedRoomMessage,
} from "../src/lib/outbound-queue-store";
import { IndexedDbDurableOutboundStore } from "../src/lib/xmpp-runtime/indexeddb-durable-store";
import { MemoryDurableOutboundStore } from "../src/lib/xmpp-runtime/memory-durable-store";
import {
  committedOrThrow,
  createOutboundClaim,
  type DurableOutboundStore,
} from "../src/lib/xmpp-runtime/durable-contract";
import type { PersistedSmResumeState } from "../src/lib/xmpp/sm-resume-types";

const ACCOUNT = "contract@example.com";
const CLOCK = { now: () => 1_000 };

function directMessage(
  id: string,
  body = "hello",
  createdAt = "2026-07-17T00:00:00.000Z",
): PersistedQueuedDmMessage {
  return {
    kind: "dm",
    id,
    createdAt,
    peerJid: "recipient@example.com",
    body,
  };
}

function roomMessage(
  id: string,
  roomJid: string,
  createdAt: string,
): PersistedQueuedRoomMessage {
  return {
    kind: "room",
    id,
    createdAt,
    roomJid,
    body: id,
  };
}

function smState(previd: string): PersistedSmResumeState {
  return {
    previd,
    inboundH: 4,
    outboundH: 7,
    maxResumeSeconds: 300,
    unhandledOutboundEntries: [],
  };
}

type ContractHarness = {
  store: DurableOutboundStore;
  close(): Promise<void>;
};

const contractAdapters: Array<{
  name: string;
  create(): ContractHarness;
}> = [
  {
    name: "memory",
    create: () => ({
      store: new MemoryDurableOutboundStore(CLOCK),
      close: async () => undefined,
    }),
  },
  {
    name: "IndexedDB",
    create: () => {
      const store = new IndexedDbDurableOutboundStore({
        authorityClock: CLOCK,
        indexedDb: new IDBFactory(),
        databaseName: `contract-${crypto.randomUUID()}`,
      });
      return { store, close: () => store.close() };
    },
  },
];

for (const adapter of contractAdapters) {
  describe(`${adapter.name} durable runtime contract`, () => {
    test("commits one canonical row and rejects a changed retry payload", async () => {
      const harness = adapter.create();
      try {
        const inserted = committedOrThrow(
          "insert",
          await harness.store.persistReady(ACCOUNT, directMessage("message-1")),
        );
        const existing = committedOrThrow(
          "existing",
          await harness.store.persistReady(ACCOUNT, directMessage("message-1")),
        );
        const invalidRetry = await harness.store.persistReady(
          ACCOUNT,
          directMessage("message-1", "hello", "2026-07-17T00:00:00Z"),
        );
        const conflict = committedOrThrow(
          "conflict",
          await harness.store.persistReady(
            ACCOUNT,
            directMessage("message-1", "changed"),
          ),
        );

        expect(inserted.kind).toBe("inserted");
        expect(existing.kind).toBe("existing");
        expect(invalidRetry.kind).toBe("failed");
        if (invalidRetry.kind !== "failed") {
          throw new Error("expected invalid timestamp failure");
        }
        expect(invalidRetry.cause).toBeInstanceOf(DOMException);
        expect((invalidRetry.cause as DOMException).name).toBe("DataError");
        expect(conflict.kind).toBe("conflict");
        expect(committedOrThrow(
          "list",
          await harness.store.list(ACCOUNT),
        )).toEqual([directMessage("message-1")]);
      } finally {
        await harness.close();
      }
    });

    test("commits monotonic lane order through rollback, future clocks, and exhaustion", async () => {
      const harness = adapter.create();
      try {
        const first = directMessage(
          "z-first",
          "first",
          "2030-01-01T00:00:00.000Z",
        );
        const rollback = directMessage(
          "a-rollback",
          "rollback",
          "2026-01-01T00:00:00.000Z",
        );
        const sameTick = directMessage(
          "b-same-tick",
          "same tick",
          "2030-01-01T00:00:00.000Z",
        );
        expect(committedOrThrow(
          "monotonic-first",
          await harness.store.persistReady(ACCOUNT, first),
        ).kind).toBe("inserted");
        const rollbackResult = committedOrThrow(
          "monotonic-rollback",
          await harness.store.persistReady(ACCOUNT, rollback),
        );
        const sameTickResult = committedOrThrow(
          "monotonic-same-tick",
          await harness.store.persistReady(ACCOUNT, sameTick),
        );
        expect(rollbackResult).toMatchObject({
          kind: "inserted",
          entry: {
            message: {
              id: "a-rollback",
              createdAt: "2030-01-01T00:00:00.001Z",
            },
          },
        });
        expect(sameTickResult).toMatchObject({
          kind: "inserted",
          entry: {
            message: {
              id: "b-same-tick",
              createdAt: "2030-01-01T00:00:00.002Z",
            },
          },
        });
        expect(committedOrThrow(
          "monotonic-retry",
          await harness.store.persistReady(ACCOUNT, rollback),
        ).kind).toBe("existing");

        const futureRoom = committedOrThrow(
          "future-room",
          await harness.store.persistReady(
            ACCOUNT,
            roomMessage(
              "future-room",
              "General@MUC.Example/Nick",
              "2040-01-01T00:00:00.000Z",
            ),
          ),
        );
        expect(futureRoom).toMatchObject({
          kind: "inserted",
          entry: {
            lane: { kind: "room", roomJid: "general@muc.example" },
            message: { roomJid: "general@muc.example" },
          },
        });
        expect(committedOrThrow(
          "future-room-canonical-retry",
          await harness.store.persistReady(
            ACCOUNT,
            roomMessage(
              "future-room",
              "general@muc.example",
              "2040-01-01T00:00:00.000Z",
            ),
          ),
        ).kind).toBe("existing");
        const isolated = committedOrThrow(
          "isolated-room",
          await harness.store.persistReady(
            ACCOUNT,
            roomMessage(
              "isolated-room",
              "random@muc.example/Other",
              "2025-01-01T00:00:00.000Z",
            ),
          ),
        );
        expect(isolated).toMatchObject({
          kind: "inserted",
          entry: {
            lane: { kind: "room", roomJid: "random@muc.example" },
            message: {
              roomJid: "random@muc.example",
              createdAt: "2025-01-01T00:00:00.000Z",
            },
          },
        });

        const beforeInvalid = committedOrThrow(
          "before-invalid-created-at",
          await harness.store.list(ACCOUNT),
        );
        for (const createdAt of [
          "not-a-timestamp",
          "2026-01-01T00:00:00Z",
          "+010000-01-01T00:00:00.000Z",
        ]) {
          const invalid = await harness.store.persistReady(
            ACCOUNT,
            directMessage(`invalid-${createdAt}`, "invalid", createdAt),
          );
          expect(invalid.kind).toBe("failed");
          if (invalid.kind !== "failed") {
            throw new Error("expected invalid timestamp failure");
          }
          expect(invalid.cause).toBeInstanceOf(DOMException);
          expect((invalid.cause as DOMException).name).toBe("DataError");
        }
        expect(committedOrThrow(
          "after-invalid-created-at",
          await harness.store.list(ACCOUNT),
        )).toEqual(beforeInvalid);

        expect(committedOrThrow(
          "exhausted-head",
          await harness.store.persistReady(
            ACCOUNT,
            roomMessage(
              "exhausted-head",
              "exhausted@muc.example",
              "9999-12-31T23:59:59.999Z",
            ),
          ),
        ).kind).toBe("inserted");
        const beforeExhaustion = committedOrThrow(
          "before-exhaustion",
          await harness.store.list(ACCOUNT),
        );
        const exhausted = await harness.store.persistReady(
          ACCOUNT,
          roomMessage(
            "exhausted-successor",
            "exhausted@muc.example/Resource",
            "2026-01-01T00:00:00.000Z",
          ),
        );
        expect(exhausted).toMatchObject({ kind: "failed", reason: "aborted" });
        if (exhausted.kind !== "failed") {
          throw new Error("expected exhausted append failure");
        }
        expect(exhausted.cause).toBeInstanceOf(DOMException);
        expect((exhausted.cause as DOMException).name).toBe("AbortError");
        expect(committedOrThrow(
          "after-exhaustion",
          await harness.store.list(ACCOUNT),
        )).toEqual(beforeExhaustion);
      } finally {
        await harness.close();
      }
    });

    test("serializes live admission against lane-head claiming with one owner", async () => {
      const harness = adapter.create();
      try {
        const owner = committedOrThrow(
          "activate-racing-claim",
          await harness.store.claimOwner(ACCOUNT, {
            ownerId: "racing-owner",
            ownerInstanceId: "racing-instance",
          }),
        ).fence;
        const message = directMessage("racing-claim");
        expect(committedOrThrow(
          "persist-racing-ready",
          await harness.store.persistReady(ACCOUNT, message),
        ).kind).toBe("inserted");

        const results = await Promise.all([
          harness.store.persistAndClaimLaneHead(
            ACCOUNT,
            message,
            {
              ...createOutboundClaim(owner, 1, "sending"),
              claimId: "live-admission",
            },
          ),
          harness.store.claimHead(
            ACCOUNT,
            { kind: "direct" },
            {
              ...createOutboundClaim(owner, 1, "sending"),
              claimId: "lane-drain",
            },
          ),
        ]);
        const values = results.map((result) => (
          committedOrThrow("racing-claim", result)
        ));
        expect(values.map(({ kind }) => kind).sort()).toEqual([
          "busy",
          "claimed",
        ]);
        const claimed = values.find((value) => value.kind === "claimed");
        if (!claimed || claimed.kind !== "claimed") {
          throw new Error("expected one canonical racing claim");
        }
        expect(["live-admission", "lane-drain"]).toContain(claimed.claim.claimId);
      } finally {
        await harness.close();
      }
    });

    test("commits claim, terminal receipt, and row deletion atomically", async () => {
      const harness = adapter.create();
      try {
        const owner = committedOrThrow(
          "activate",
          await harness.store.claimOwner(ACCOUNT, {
            ownerId: "contract-owner",
            ownerInstanceId: "contract-instance",
          }),
        ).fence;
        await harness.store.persistReady(ACCOUNT, directMessage("terminal-1"));
        const claimed = committedOrThrow(
          "claim",
          await harness.store.claimHead(
            ACCOUNT,
            { kind: "direct" },
            createOutboundClaim(owner, 1, "sending"),
          ),
        );
        if (claimed.kind !== "claimed") {
          throw new Error(`expected claimed row, received ${claimed.kind}`);
        }
        const recorded = committedOrThrow(
          "record-terminal",
          await harness.store.recordTerminal(
            claimed.entry.identity,
            "ack",
            claimed.claim,
          ),
        );
        if (recorded.kind !== "recorded") {
          throw new Error(`expected terminal receipt, received ${recorded.kind}`);
        }
        expect(committedOrThrow(
          "apply-terminal",
          await harness.store.applyTerminal(owner, recorded.intent),
        ).kind).toBe("acked");
        expect(committedOrThrow(
          "list-after-terminal",
          await harness.store.list(ACCOUNT),
        )).toEqual([]);
        expect(committedOrThrow(
          "terminal-after-apply",
          await harness.store.listTerminal(ACCOUNT),
        )).toEqual([]);
      } finally {
        await harness.close();
      }
    });

    test("enforces SM compare-and-set versions without partial mutation", async () => {
      const harness = adapter.create();
      try {
        const owner = committedOrThrow(
          "activate-sm",
          await harness.store.claimOwner(ACCOUNT, {
            ownerId: "sm-owner",
            ownerInstanceId: "sm-instance",
          }),
        ).fence;
        const saved = committedOrThrow(
          "save-sm",
          await harness.store.saveSm(owner, null, smState("resume-1"), 1_000),
        );
        if (saved.kind !== "applied") throw new Error("expected SM save");
        expect(committedOrThrow(
          "stale-sm-save",
          await harness.store.saveSm(owner, null, smState("stale"), 1_001),
        )).toEqual({ kind: "stale", actualVersion: saved.value.version });
        const loaded = committedOrThrow(
          "load-sm",
          await harness.store.loadSm(owner),
        );
        expect(loaded.kind === "loaded" && loaded.envelope?.state.previd).toBe(
          "resume-1",
        );
      } finally {
        await harness.close();
      }
    });
  });
}

function openRawDatabase(
  indexedDb: IDBFactory,
  name: string,
  version?: number,
  upgrade?: (database: IDBDatabase) => void,
): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = version === undefined
      ? indexedDb.open(name)
      : indexedDb.open(name, version);
    request.onupgradeneeded = () => upgrade?.(request.result);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
    request.onblocked = () => reject(
      new DOMException("raw IndexedDB open blocked", "AbortError"),
    );
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

function deleteRawDatabase(
  indexedDb: IDBFactory,
  name: string,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const request = indexedDb.deleteDatabase(name);
    request.onsuccess = () => resolve();
    request.onerror = () => reject(request.error);
    request.onblocked = () => reject(
      new DOMException("raw IndexedDB delete blocked", "AbortError"),
    );
  });
}

function countDatabaseOpens(indexedDb: IDBFactory): () => number {
  const open = indexedDb.open.bind(indexedDb);
  let count = 0;
  Object.defineProperty(indexedDb, "open", {
    configurable: true,
    value: (name: string, version?: number) => {
      count += 1;
      return version === undefined
        ? open(name)
        : open(name, version);
    },
  });
  return () => count;
}

async function putRawAccount(
  database: IDBDatabase,
  value: Record<string, unknown>,
): Promise<void> {
  const transaction = database.transaction("accounts", "readwrite");
  transaction.objectStore("accounts").put(value);
  await transactionDone(transaction);
}

async function countRawAccounts(database: IDBDatabase): Promise<number> {
  const transaction = database.transaction("accounts", "readonly");
  const request = transaction.objectStore("accounts").count();
  const count = await new Promise<number>((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
  await transactionDone(transaction);
  return count;
}

describe("IndexedDB durable repository transaction boundaries", () => {
  test("serializes two store instances racing to persist the same row", async () => {
    const indexedDb = new IDBFactory();
    const databaseName = `race-${crypto.randomUUID()}`;
    const options = {
      authorityClock: CLOCK,
      indexedDb,
      databaseName,
    };
    const firstStore = new IndexedDbDurableOutboundStore(options);
    const secondStore = new IndexedDbDurableOutboundStore(options);
    try {
      const outcomes = await Promise.all([
        firstStore.persistReady(ACCOUNT, directMessage("race-1")),
        secondStore.persistReady(ACCOUNT, directMessage("race-1")),
      ]);
      expect(outcomes.map((outcome) => committedOrThrow(
        "racing-persist",
        outcome,
      ).kind).sort()).toEqual(["existing", "inserted"]);
      expect(committedOrThrow(
        "race-list",
        await secondStore.list(ACCOUNT),
      )).toEqual([directMessage("race-1")]);
    } finally {
      await Promise.all([firstStore.close(), secondStore.close()]);
    }
  });

  test("aborts a transaction that reads corrupt state without repairing it", async () => {
    const indexedDb = new IDBFactory();
    const databaseName = `abort-${crypto.randomUUID()}`;
    const raw = await openRawDatabase(indexedDb, databaseName, 1, (database) => {
      database.createObjectStore("accounts", { keyPath: "accountKey" });
    });
    await putRawAccount(raw, {
      accountKey: ACCOUNT,
      schemaVersion: 1,
      unexpectedRepairMarker: true,
    });
    raw.close();

    const store = new IndexedDbDurableOutboundStore({
      authorityClock: CLOCK,
      indexedDb,
      databaseName,
    });
    const outcome = await store.revision(ACCOUNT);
    expect(outcome.kind).toBe("failed");
    if (outcome.kind === "failed") {
      expect(outcome.cause).toBeInstanceOf(DOMException);
      expect((outcome.cause as DOMException).name).toBe("DataError");
    }
    await store.close();

    const persisted = await openRawDatabase(indexedDb, databaseName);
    expect(await countRawAccounts(persisted)).toBe(1);
    persisted.close();
  });

  test("reports a write failure only after the transaction aborts", async () => {
    const indexedDb = new IDBFactory();
    const databaseName = `write-failure-${crypto.randomUUID()}`;
    const malformed = await openRawDatabase(
      indexedDb,
      databaseName,
      1,
      (database) => {
        database.createObjectStore("accounts", { keyPath: "wrongKey" });
      },
    );
    malformed.close();

    const store = new IndexedDbDurableOutboundStore({
      authorityClock: CLOCK,
      indexedDb,
      databaseName,
    });
    const outcome = await store.persistReady(
      ACCOUNT,
      directMessage("write-failure-1"),
    );
    expect(outcome.kind).toBe("failed");
    if (outcome.kind === "failed") {
      expect(outcome.cause).toBeInstanceOf(DOMException);
      expect((outcome.cause as DOMException).name).toBe("DataError");
    }
    await store.close();

    const persisted = await openRawDatabase(indexedDb, databaseName);
    expect(await countRawAccounts(persisted)).toBe(0);
    persisted.close();
  });

  test("upgrades a populated v1 database to v2 without losing durable rows", async () => {
    const indexedDb = new IDBFactory();
    const databaseName = `upgrade-${crypto.randomUUID()}`;
    const v1 = new IndexedDbDurableOutboundStore({
      authorityClock: CLOCK,
      indexedDb,
      databaseName,
      databaseVersion: 1,
    });
    expect(committedOrThrow(
      "v1-persist",
      await v1.persistReady(ACCOUNT, directMessage("survives-v2-upgrade")),
    ).kind).toBe("inserted");
    await v1.close();

    const v2 = new IndexedDbDurableOutboundStore({
      authorityClock: CLOCK,
      indexedDb,
      databaseName,
      databaseVersion: 2,
    });
    expect(committedOrThrow(
      "v2-list",
      await v2.list(ACCOUNT),
    )).toEqual([directMessage("survives-v2-upgrade")]);
    await v2.close();

    const upgraded = await openRawDatabase(indexedDb, databaseName);
    expect(upgraded.version).toBe(2);
    expect(upgraded.objectStoreNames.contains("accounts")).toBe(true);
    expect(await countRawAccounts(upgraded)).toBe(1);
    upgraded.close();
  });

  test("reuses one cached connection until explicit close forces a reopen", async () => {
    const indexedDb = new IDBFactory();
    const openCount = countDatabaseOpens(indexedDb);
    const store = new IndexedDbDurableOutboundStore({
      authorityClock: CLOCK,
      indexedDb,
      databaseName: `cached-${crypto.randomUUID()}`,
    });

    expect(committedOrThrow("cached-revision", await store.revision(ACCOUNT))).toBe(0);
    expect(committedOrThrow("cached-list", await store.list(ACCOUNT))).toEqual([]);
    expect(openCount()).toBe(1);

    await store.close();
    expect(committedOrThrow("reopened-revision", await store.revision(ACCOUNT))).toBe(0);
    expect(openCount()).toBe(2);
    await store.close();
  });

  test("external versionchange closes and clears the cached connection", async () => {
    const indexedDb = new IDBFactory();
    const openCount = countDatabaseOpens(indexedDb);
    const databaseName = `versionchange-${crypto.randomUUID()}`;
    const store = new IndexedDbDurableOutboundStore({
      authorityClock: CLOCK,
      indexedDb,
      databaseName,
    });

    expect(committedOrThrow(
      "versionchange-initial-open",
      await store.revision(ACCOUNT),
    )).toBe(0);
    expect(openCount()).toBe(1);

    await deleteRawDatabase(indexedDb, databaseName);
    expect(committedOrThrow(
      "versionchange-reopen",
      await store.revision(ACCOUNT),
    )).toBe(0);
    expect(openCount()).toBe(2);
    await store.close();
  });

  test("fails a blocked schema upgrade closed instead of hanging", async () => {
    const indexedDb = new IDBFactory();
    const databaseName = `blocked-${crypto.randomUUID()}`;
    const blocker = await openRawDatabase(indexedDb, databaseName, 1, (database) => {
      database.createObjectStore("accounts", { keyPath: "accountKey" });
    });
    const store = new IndexedDbDurableOutboundStore({
      authorityClock: CLOCK,
      indexedDb,
      databaseName,
      databaseVersion: 2,
    });

    const outcome = await store.revision(ACCOUNT);
    expect(outcome.kind).toBe("failed");
    if (outcome.kind === "failed") {
      expect(outcome.reason).toBe("aborted");
      expect(outcome.cause).toBeInstanceOf(DOMException);
      expect((outcome.cause as DOMException).name).toBe("AbortError");
    }

    blocker.close();
    await store.close();
  });
});
