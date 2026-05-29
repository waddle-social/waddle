// Pins the account+service-scoped persistence of the XEP-0050
// `register-device` outcome (push node id + Push Service device id).
// The scoping invariant matters on a shared browser: account B must not
// read back account A's node/device ids (which would drive a
// `disable-device` against a row B doesn't own). Also covers the
// disable-flow cleanup contract (clear leaves no stale ids) and SSR
// safety (no `window`).

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  clearDeviceId,
  clearPushNodeId,
  loadDeviceId,
  loadPushNodeId,
  persistDeviceId,
  persistPushNodeId,
} from "../src/shell/push-device-store";

// Bun's test runtime has no `window.localStorage`; install an in-memory
// shim before each test (mirrors `vapid-cache.test.ts`).
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
}

function uninstallLocalStorage() {
  delete (globalThis as { window?: unknown }).window;
}

const ACCOUNT = "alice@example.com";
const SERVICE = "push.example.com";
const OTHER_ACCOUNT = "bob@example.com";
const OTHER_SERVICE = "push.other.example.com";

describe("push-device-store", () => {
  beforeEach(installLocalStorage);
  afterEach(uninstallLocalStorage);

  test("round-trips node + device id for an (account, service) pair", () => {
    persistPushNodeId(ACCOUNT, SERVICE, "node-1");
    persistDeviceId(ACCOUNT, SERVICE, "device-1");
    expect(loadPushNodeId(ACCOUNT, SERVICE)).toBe("node-1");
    expect(loadDeviceId(ACCOUNT, SERVICE)).toBe("device-1");
  });

  test("returns null when nothing is persisted", () => {
    expect(loadPushNodeId(ACCOUNT, SERVICE)).toBeNull();
    expect(loadDeviceId(ACCOUNT, SERVICE)).toBeNull();
  });

  test("isolates ids by account — no cross-account read on a shared browser", () => {
    persistDeviceId(ACCOUNT, SERVICE, "alice-device");
    persistPushNodeId(ACCOUNT, SERVICE, "alice-node");
    expect(loadDeviceId(OTHER_ACCOUNT, SERVICE)).toBeNull();
    expect(loadPushNodeId(OTHER_ACCOUNT, SERVICE)).toBeNull();
  });

  test("isolates ids by service", () => {
    persistDeviceId(ACCOUNT, SERVICE, "device-1");
    persistPushNodeId(ACCOUNT, SERVICE, "node-1");
    expect(loadDeviceId(ACCOUNT, OTHER_SERVICE)).toBeNull();
    expect(loadPushNodeId(ACCOUNT, OTHER_SERVICE)).toBeNull();
  });

  test("clear removes only the targeted pair's ids (disable-flow cleanup)", () => {
    persistDeviceId(ACCOUNT, SERVICE, "alice-device");
    persistPushNodeId(ACCOUNT, SERVICE, "alice-node");
    persistDeviceId(OTHER_ACCOUNT, SERVICE, "bob-device");
    persistPushNodeId(OTHER_ACCOUNT, SERVICE, "bob-node");

    clearDeviceId(ACCOUNT, SERVICE);
    clearPushNodeId(ACCOUNT, SERVICE);

    // The disabling account's ids are gone — no stale state left behind.
    expect(loadDeviceId(ACCOUNT, SERVICE)).toBeNull();
    expect(loadPushNodeId(ACCOUNT, SERVICE)).toBeNull();
    // A co-tenant on the same browser is untouched.
    expect(loadDeviceId(OTHER_ACCOUNT, SERVICE)).toBe("bob-device");
    expect(loadPushNodeId(OTHER_ACCOUNT, SERVICE)).toBe("bob-node");
  });

  test("SSR-safe: with no window, load is null and persist/clear are no-ops", () => {
    uninstallLocalStorage();
    expect(() => persistDeviceId(ACCOUNT, SERVICE, "device-1")).not.toThrow();
    expect(() => persistPushNodeId(ACCOUNT, SERVICE, "node-1")).not.toThrow();
    expect(loadDeviceId(ACCOUNT, SERVICE)).toBeNull();
    expect(loadPushNodeId(ACCOUNT, SERVICE)).toBeNull();
    expect(() => clearDeviceId(ACCOUNT, SERVICE)).not.toThrow();
    expect(() => clearPushNodeId(ACCOUNT, SERVICE)).not.toThrow();
  });
});
