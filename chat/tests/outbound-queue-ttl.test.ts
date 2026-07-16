/**
 * PR4 — outbound queue TTL.
 *
 * A queued message that fails to ack (e.g. user typed it, closed
 * the tab before sending, then opened a new tab weeks later) used
 * to replay forever with its frozen `createdAt`, inserting ahead
 * of every message received since and surprising the user with a
 * send they don't remember intending. The store now prunes
 * entries older than 7 days on read so the persisted footprint
 * stays bounded over the lifetime of the account.
 */
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  countQueuedMessages,
  enqueueQueuedMessage,
  listQueuedDmMessages,
} from "../src/lib/outbound-queue-store";

function createStorageMock() {
  const values = new Map<string, string>();
  return {
    getItem(key: string) { return values.has(key) ? values.get(key)! : null; },
    setItem(key: string, value: string) { values.set(key, value); },
    removeItem(key: string) { values.delete(key); },
    clear() { values.clear(); },
    has(key: string) { return values.has(key); },
    key(index: number) { return [...values.keys()][index] ?? null; },
    get length() { return values.size; },
  };
}

const originalWindow = globalThis.window;
const originalLocalStorage = globalThis.localStorage;
let storage: ReturnType<typeof createStorageMock>;

beforeEach(() => {
  storage = createStorageMock();
  (globalThis as typeof globalThis & { localStorage: typeof storage }).localStorage = storage;
  (globalThis as typeof globalThis & { window: Window & { localStorage: typeof storage } }).window = {
    ...(originalWindow ?? {}),
    localStorage: storage,
  } as Window & { localStorage: typeof storage };
});

afterEach(() => {
  storage.clear();
  if (originalLocalStorage === undefined) {
    Reflect.deleteProperty(globalThis, "localStorage");
  } else {
    (globalThis as typeof globalThis & { localStorage: Storage }).localStorage = originalLocalStorage;
  }
  if (originalWindow === undefined) {
    Reflect.deleteProperty(globalThis, "window");
  } else {
    (globalThis as typeof globalThis & { window: Window & typeof globalThis }).window = originalWindow;
  }
});

function isoDaysAgo(days: number): string {
  return new Date(Date.now() - days * 24 * 60 * 60 * 1000).toISOString();
}

describe("outbound queue TTL", () => {
  test("entries within the 7-day window are preserved", () => {
    enqueueQueuedMessage("alice@example.com", {
      kind: "dm",
      id: "dm-fresh",
      createdAt: isoDaysAgo(3),
      peerJid: "bob@example.com",
      body: "still in window",
    });
    expect(listQueuedDmMessages("alice@example.com", "bob@example.com", "account")).toHaveLength(1);
  });

  test("entries older than 7 days are pruned on read", () => {
    enqueueQueuedMessage("alice@example.com", {
      kind: "dm",
      id: "dm-stale",
      createdAt: isoDaysAgo(30),
      peerJid: "bob@example.com",
      body: "ancient draft",
    });
    expect(listQueuedDmMessages("alice@example.com", "bob@example.com", "account")).toEqual([]);
  });

  test("pruning rewrites the persisted snapshot (the entry is gone, not just hidden)", () => {
    enqueueQueuedMessage("alice@example.com", {
      kind: "dm",
      id: "dm-stale",
      createdAt: isoDaysAgo(30),
      peerJid: "bob@example.com",
      body: "ancient draft",
    });
    enqueueQueuedMessage("alice@example.com", {
      kind: "dm",
      id: "dm-fresh",
      createdAt: isoDaysAgo(1),
      peerJid: "bob@example.com",
      body: "recent draft",
    });

    // Reading triggers prune.
    expect(countQueuedMessages("alice@example.com")).toBe(1);
    // The persisted blob should reflect just the fresh entry now.
    const raw = storage.getItem("waddle.chat.outbound-queue.v2.17:alice@example.com.dm-fresh");
    expect(raw).not.toBeNull();
    const parsed = JSON.parse(raw!);
    expect(parsed.id).toBe("dm-fresh");
  });

  test("when every entry is stale the storage key is removed entirely", () => {
    enqueueQueuedMessage("alice@example.com", {
      kind: "dm",
      id: "dm-stale-1",
      createdAt: isoDaysAgo(30),
      peerJid: "bob@example.com",
      body: "ancient 1",
    });
    enqueueQueuedMessage("alice@example.com", {
      kind: "dm",
      id: "dm-stale-2",
      createdAt: isoDaysAgo(60),
      peerJid: "bob@example.com",
      body: "ancient 2",
    });

    expect(countQueuedMessages("alice@example.com")).toBe(0);
    expect(storage.has("waddle.chat.outbound-queue.v2.17:alice@example.com.dm-stale-1")).toBe(false);
    expect(storage.has("waddle.chat.outbound-queue.v2.17:alice@example.com.dm-stale-2")).toBe(false);
  });

  test("unparseable createdAt is treated as fresh (refuse-to-prune is safer than dropping)", () => {
    enqueueQueuedMessage("alice@example.com", {
      kind: "dm",
      id: "dm-mystery",
      createdAt: "not-a-date",
      peerJid: "bob@example.com",
      body: "weird stamp",
    });
    expect(listQueuedDmMessages("alice@example.com", "bob@example.com", "account")).toHaveLength(1);
  });
});
