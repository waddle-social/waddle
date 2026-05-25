// Coverage for the runtime VAPID cache that replaced the build-time
// `PUBLIC_WADDLE_VAPID_PUBLIC_KEY` env. Pins:
//   * cache miss → wasm fetch → write-through
//   * cache hit within the TTL skips the fetch
//   * cache miss past the TTL re-fetches
//   * `forceRefresh: true` always fetches
//   * server-not-advertised (fetch resolves null) → null + no cache write

import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import {
  clearCachedVapidKey,
  loadVapidPublicKey,
  VAPID_CACHE_MAX_AGE_MS,
} from "../src/shell/vapid-cache";

// Minimal localStorage stub. Bun's test runtime does not provide a
// browser `window.localStorage` by default, so we install a fresh
// in-memory shim before each test.
function installLocalStorage() {
  const store = new Map<string, string>();
  const stub = {
    getItem(key: string) {
      return store.has(key) ? store.get(key) ?? null : null;
    },
    setItem(key: string, value: string) {
      store.set(key, value);
    },
    removeItem(key: string) {
      store.delete(key);
    },
    clear() {
      store.clear();
    },
    key(_index: number) {
      return null;
    },
    get length() {
      return store.size;
    },
  };
  (globalThis as { window?: { localStorage: typeof stub } }).window = { localStorage: stub };
  return stub;
}

function uninstallLocalStorage() {
  delete (globalThis as { window?: unknown }).window;
}

function makeClient(advertisement: { publicKey: string; kid: string } | null) {
  return {
    fetchVapidPublicKey: mock(async (_opts: { serviceJid: string }) => advertisement),
  } as unknown as Parameters<typeof loadVapidPublicKey>[0]["client"];
}

const ACCOUNT = "alice@example.com";
const SERVER = "push.example.com";
const SAMPLE = {
  publicKey: "BNcRdreALRFXTkOOUHK1EtK2wtZ09Jx_QqOmuyYxLBxbu1zdMrTaUaiyB6e2BIcL4XPzG4Ovq7BU8a-zsnNvBxg",
  kid: "9b1f0f3e-1234-4abc-9001-00112233aabb",
};

describe("vapid-cache", () => {
  beforeEach(() => {
    installLocalStorage();
  });
  afterEach(() => {
    uninstallLocalStorage();
  });

  test("cache miss fetches once, then hit serves from storage", async () => {
    const client = makeClient(SAMPLE);
    let now = 1_000_000;

    const first = await loadVapidPublicKey({
      client,
      accountJid: ACCOUNT,
      serverJid: SERVER,
      now: () => now,
    });
    expect(first?.publicKey).toBe(SAMPLE.publicKey);
    expect(first?.kid).toBe(SAMPLE.kid);
    expect(first?.fetchedAtMs).toBe(now);

    // Bump the clock under the TTL; the cached value should serve
    // without a fresh wasm call.
    now += VAPID_CACHE_MAX_AGE_MS - 1;
    const second = await loadVapidPublicKey({
      client,
      accountJid: ACCOUNT,
      serverJid: SERVER,
      now: () => now,
    });
    expect(second?.publicKey).toBe(SAMPLE.publicKey);
    expect((client.fetchVapidPublicKey as ReturnType<typeof mock>).mock.calls).toHaveLength(1);
  });

  test("cache miss past the TTL re-fetches", async () => {
    const client = makeClient(SAMPLE);
    let now = 1_000_000;
    await loadVapidPublicKey({ client, accountJid: ACCOUNT, serverJid: SERVER, now: () => now });
    now += VAPID_CACHE_MAX_AGE_MS + 1;
    await loadVapidPublicKey({ client, accountJid: ACCOUNT, serverJid: SERVER, now: () => now });
    expect((client.fetchVapidPublicKey as ReturnType<typeof mock>).mock.calls).toHaveLength(2);
  });

  test("forceRefresh bypasses cache", async () => {
    const client = makeClient(SAMPLE);
    const now = 1_000_000;
    await loadVapidPublicKey({ client, accountJid: ACCOUNT, serverJid: SERVER, now: () => now });
    await loadVapidPublicKey({
      client,
      accountJid: ACCOUNT,
      serverJid: SERVER,
      forceRefresh: true,
      now: () => now,
    });
    expect((client.fetchVapidPublicKey as ReturnType<typeof mock>).mock.calls).toHaveLength(2);
  });

  test("server-not-advertised resolves null and does not write the cache", async () => {
    const client = makeClient(null);
    const now = 1_000_000;
    const result = await loadVapidPublicKey({
      client,
      accountJid: ACCOUNT,
      serverJid: SERVER,
      now: () => now,
    });
    expect(result).toBeNull();
    // No cache entry written — verify by switching to a server that
    // returns the SAMPLE; second call must fetch (not serve a stale
    // null from cache).
    const client2 = makeClient(SAMPLE);
    const second = await loadVapidPublicKey({
      client: client2,
      accountJid: ACCOUNT,
      serverJid: SERVER,
      now: () => now,
    });
    expect(second?.publicKey).toBe(SAMPLE.publicKey);
  });

  test("clearCachedVapidKey removes the entry", async () => {
    const client = makeClient(SAMPLE);
    const now = 1_000_000;
    await loadVapidPublicKey({ client, accountJid: ACCOUNT, serverJid: SERVER, now: () => now });
    clearCachedVapidKey(ACCOUNT, SERVER);
    await loadVapidPublicKey({ client, accountJid: ACCOUNT, serverJid: SERVER, now: () => now });
    expect((client.fetchVapidPublicKey as ReturnType<typeof mock>).mock.calls).toHaveLength(2);
  });

  test("cache key isolates by (account, server)", async () => {
    const client = makeClient(SAMPLE);
    const now = 1_000_000;
    await loadVapidPublicKey({ client, accountJid: ACCOUNT, serverJid: SERVER, now: () => now });
    // Same account, different server: must fetch.
    await loadVapidPublicKey({
      client,
      accountJid: ACCOUNT,
      serverJid: "push.other.example.com",
      now: () => now,
    });
    // Same server, different account: must fetch.
    await loadVapidPublicKey({
      client,
      accountJid: "bob@example.com",
      serverJid: SERVER,
      now: () => now,
    });
    expect((client.fetchVapidPublicKey as ReturnType<typeof mock>).mock.calls).toHaveLength(3);
  });

  test("malformed cache entry is treated as a miss", async () => {
    // Pre-existing localStorage entry under the CURRENT key shape but
    // with un-parseable JSON content must NOT crash the loader; the
    // entry is ignored and the next fetch overwrites. Uses the same
    // key encoding (`JSON.stringify([account, server])`) as the
    // production code at `storageKey(...)`, so the path actually
    // exercised here is `JSON.parse` raising, NOT a cache miss.
    const ls = (globalThis as { window: { localStorage: { setItem: (k: string, v: string) => void } } })
      .window.localStorage;
    ls.setItem(`waddle.chat.vapid-cache:${JSON.stringify([ACCOUNT, SERVER])}`, "not json");
    const client = makeClient(SAMPLE);
    const result = await loadVapidPublicKey({
      client,
      accountJid: ACCOUNT,
      serverJid: SERVER,
      now: () => 1_000_000,
    });
    expect(result?.publicKey).toBe(SAMPLE.publicKey);
    expect((client.fetchVapidPublicKey as ReturnType<typeof mock>).mock.calls).toHaveLength(1);
  });

  test("partially-valid cache entry (missing fields) is treated as a miss", async () => {
    // Schema drift: an entry that JSON.parses successfully but lacks
    // required fields must fail the `isCachedVapidEntry` guard and
    // trigger a fresh fetch rather than propagating an unsafe
    // partial value.
    const ls = (globalThis as { window: { localStorage: { setItem: (k: string, v: string) => void } } })
      .window.localStorage;
    ls.setItem(
      `waddle.chat.vapid-cache:${JSON.stringify([ACCOUNT, SERVER])}`,
      JSON.stringify({ publicKey: "x" }),
    );
    const client = makeClient(SAMPLE);
    const result = await loadVapidPublicKey({
      client,
      accountJid: ACCOUNT,
      serverJid: SERVER,
      now: () => 1_000_000,
    });
    expect(result?.publicKey).toBe(SAMPLE.publicKey);
    expect((client.fetchVapidPublicKey as ReturnType<typeof mock>).mock.calls).toHaveLength(1);
  });
});
