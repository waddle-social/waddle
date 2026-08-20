/**
 * PR3 — persistence tests for `ReconnectCatchup` hydration and the
 * `ResumePersistence` localStorage adapter.
 *
 * Without persistence, a hard reload (cold start, mobile Safari
 * aggressive eviction) loses every per-conversation cursor, so the
 * next `session:started` returns `[]` and MAM catch-up does NOT
 * run — missed messages are only re-fetched when the user manually
 * scrolls back. This PR persists the cursor map (and the XEP-0198
 * `{previd, inboundH, outboundH}` POD) so a reload picks up where
 * the last tab left off.
 */
import { afterAll, afterEach, beforeAll, describe, expect, test } from "bun:test";
import { ReconnectCatchup } from "../src/lib/xmpp/reconnect-catchup";
import { BrowserXmppClient } from "../src/lib/xmpp/client";
import { applyResumeStateToWasmConfig } from "../src/lib/xmpp/client-connection";
import { enqueueQueuedMessage, listQueuedDmMessages, removeQueuedMessage } from "../src/lib/outbound-queue-store";
import type { WaddleSession } from "../src/lib/server-auth";
import {
  createLocalStorageResumePersistence,
  nullResumePersistence,
  resumeOwnerLifecycleSnapshotForTests,
  type PersistedReconnectCatchup,
  type PersistedSmResumeState,
  type ResumePersistence,
} from "../src/lib/xmpp/resume-persistence";

// Bun's default test env is Node-like (no `window` / `localStorage`).
// Install a minimal Map-backed Storage polyfill so the localStorage
// adapter is exercised the same way it would be in a real browser —
// otherwise we'd be skipping the only tests that prove
// `JSON.stringify` / `JSON.parse` round-trip and TTL handling work.
//
// Scoped: installed in `beforeAll` and uninstalled in `afterAll` so
// the shim does NOT leak into other test files. Other suites
// (especially anything constructing `BrowserXmppClient`) assume
// `typeof window === "undefined"` for the no-op persistence fast
// path; a leaked shim would activate localStorage in those tests
// and corrupt state across runs.
const WINDOW_SENTINEL = Symbol("test-installed-window");
type ShimmedGlobal = typeof globalThis & {
  window?: { localStorage: Storage; sessionStorage: Storage } & { [WINDOW_SENTINEL]?: true };
};
beforeAll(() => {
  const g = globalThis as ShimmedGlobal;
  if (typeof g.window !== "undefined") return;
  const store = new Map<string, string>();
  const sessionStore = new Map<string, string>();
  const storage: Storage = {
    get length() { return store.size; },
    clear: () => store.clear(),
    getItem: (key) => store.get(key) ?? null,
    key: (index) => Array.from(store.keys())[index] ?? null,
    removeItem: (key) => { store.delete(key); },
    setItem: (key, value) => { store.set(key, String(value)); },
  };
  const sessionStorage: Storage = {
    get length() { return sessionStore.size; },
    clear: () => sessionStore.clear(),
    getItem: (key) => sessionStore.get(key) ?? null,
    key: (index) => Array.from(sessionStore.keys())[index] ?? null,
    removeItem: (key) => { sessionStore.delete(key); },
    setItem: (key, value) => { sessionStore.set(key, String(value)); },
  };
  g.window = Object.assign({ localStorage: storage, sessionStorage }, { [WINDOW_SENTINEL]: true as const });
});

afterAll(() => {
  const g = globalThis as ShimmedGlobal;
  if (g.window?.[WINDOW_SENTINEL]) {
    delete (g as { window?: unknown }).window;
  }
});

afterEach(() => {
  const g = globalThis as ShimmedGlobal;
  if (g.window?.[WINDOW_SENTINEL]) {
    g.window.localStorage.clear();
    g.window.sessionStorage.clear();
  }
});

function installNavigationTiming(type: string | null): () => void {
  const original = Object.getOwnPropertyDescriptor(globalThis, "performance");
  Object.defineProperty(globalThis, "performance", {
    configurable: true,
    value: type === null
      ? undefined
      : { getEntriesByType: () => [{ type }] },
  });
  return () => {
    if (original) {
      Object.defineProperty(globalThis, "performance", original);
    } else {
      Reflect.deleteProperty(globalThis, "performance");
    }
  };
}

function seedCopiedOwnerHandoff(
  ownerId: string,
  state: PersistedSmResumeState,
  handoffInstanceId = "previous-page",
): void {
  createLocalStorageResumePersistence("alice@example.com", ownerId).saveSm(state);
  window.sessionStorage.setItem("waddle.chat.sm-resume.owner", ownerId);
  window.localStorage.setItem(
    ownerLeaseKey("alice@example.com", ownerId),
    JSON.stringify({ ownerId, instanceId: "previous-page", updatedAt: Date.now() }),
  );
  window.localStorage.setItem(
    ownerHandoffKey("alice@example.com", ownerId),
    JSON.stringify({ ownerId, instanceId: handoffInstanceId, expiresAt: Date.now() + 45_000 }),
  );
}

function ownerRegistryKey(accountKey: string, ownerId: string): string {
  return `${accountKey.length}:${accountKey}.${ownerId.length}:${ownerId}`;
}

function ownerLeaseKey(accountKey: string, ownerId: string): string {
  return `waddle.chat.sm-resume.owner-lease.${ownerRegistryKey(accountKey, ownerId)}`;
}

function ownerHandoffKey(accountKey: string, ownerId: string): string {
  return `waddle.chat.sm-resume.owner-handoff.${ownerRegistryKey(accountKey, ownerId)}`;
}

function smOwnerKey(accountKey: string, ownerId: string): string {
  return `waddle.chat.sm-resume.${accountKey.length}:${accountKey}.${ownerId.length}:${ownerId}`;
}

function smConsumedKey(accountKey: string, ownerId: string): string {
  return `${smOwnerKey(accountKey, ownerId)}.consumed`;
}

function ownResumeKey(accountKey: string, previd: string): string {
  return `waddle.chat.sm-resume.own-resume.${accountKey.length}:${accountKey}.${previd.length}:${previd}`;
}

function ownResumeAttemptKey(
  accountKey: string,
  previd: string,
  ownerId: string,
  ownerInstanceId: string,
  clientId: string,
): string {
  return `${ownResumeKey(accountKey, previd)}.attempt.${ownerId.length}:${ownerId}.${ownerInstanceId.length}:${ownerInstanceId}.${clientId.length}:${clientId}`;
}

function readOwnResumePointer(accountKey: string, previd: string): { attemptKey: string } | null {
  const raw = window.localStorage.getItem(ownResumeKey(accountKey, previd));
  return raw ? JSON.parse(raw) as { attemptKey: string } : null;
}

function ownerTimerHarness() {
  const callbacks = new Map<number, () => void>();
  let nextId = 0;
  return {
    driver: {
      setInterval: (callback: () => void) => {
        const id = ++nextId;
        callbacks.set(id, callback);
        return id as unknown as ReturnType<typeof setInterval>;
      },
      clearInterval: (timer: ReturnType<typeof setInterval>) => {
        callbacks.delete(timer as unknown as number);
      },
    },
    activeCount: () => callbacks.size,
    callbacks: () => [...callbacks.values()],
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => { resolve = resolvePromise; });
  return { promise, resolve };
}

/** In-memory persistence for tests — same shape as the real adapter
 * but without touching localStorage. */
function inMemoryPersistence(): ResumePersistence & {
  catchupSnapshot: () => PersistedReconnectCatchup | null;
  smSnapshot: () => PersistedSmResumeState | null;
  joinedRoomsSnapshot: () => string[];
  clearSmCount: () => number;
  retainedOwnResumeCount: () => number;
} {
  let catchup: PersistedReconnectCatchup | null = null;
  let sm: PersistedSmResumeState | null = null;
  const ownResumes = new Map<string, string>();
  let joinedRooms: string[] = [];
  let smClears = 0;
  let retainedOwnResumes = 0;
  return {
    dispose: () => undefined,
    loadCatchup: () => catchup,
    saveCatchup: (snapshot) => { catchup = snapshot; },
    clearCatchup: () => { catchup = null; },
    loadSm: () => sm,
    consumeSm: () => {
      const current = sm;
      sm = null;
      return current;
    },
    saveSm: (state) => { sm = state; },
    clearSm: () => { smClears += 1; sm = null; },
    beginOwnResume: (previd, clientId) => { ownResumes.set(previd, clientId); },
    retainOwnResume: () => { retainedOwnResumes += 1; },
    finishOwnResume: (previd, clientId) => {
      if (ownResumes.get(previd) === clientId) ownResumes.delete(previd);
    },
    consumeOwnResume: (previd, clientId) => {
      const owner = ownResumes.get(previd);
      if (!owner) return "absent";
      if (owner === clientId) return "self";
      return "foreign";
    },
    preparePagehideHandoff: () => undefined,
    loadJoinedRooms: () => [...joinedRooms],
    saveJoinedRooms: (rooms) => { joinedRooms = [...rooms]; },
    clearJoinedRooms: () => { joinedRooms = []; },
    catchupSnapshot: () => catchup,
    smSnapshot: () => sm,
    joinedRoomsSnapshot: () => [...joinedRooms],
    clearSmCount: () => smClears,
    retainedOwnResumeCount: () => retainedOwnResumes,
  };
}

async function flushMicrotasks() {
  // `queueMicrotask` callbacks fire after the current sync block but
  // before the next macrotask. A zero-delay setTimeout drains them.
  await new Promise((resolve) => setTimeout(resolve, 0));
}

function catchupShardKey(accountKey: string, ownerId: string): string {
  return `waddle.chat.resume-cursors.${accountKey.length}:${accountKey}.${ownerId}`;
}

describe("ReconnectCatchup persistence — hydrate from snapshot on construct", () => {
  test("hydrates DM + room cursors from the persistence snapshot", () => {
    const persistence = inMemoryPersistence();
    persistence.saveCatchup({
      dmLastSeen: [
        ["bob@example.com", { timestamp: "2026-05-20T10:00:00.000Z", archiveId: "dm-arch-1" }],
      ],
      roomLastSeen: [
        ["general@muc.example.com", { timestamp: "2026-05-20T11:00:00.000Z", seenIds: ["r-1", "r-2"] }],
      ],
    });

    const c = new ReconnectCatchup(persistence);

    expect(c.getDmLastSeen("bob@example.com")).toBe("2026-05-20T10:00:00.000Z");
    expect(c.getRoomLastSeen("general@muc.example.com")).toBe("2026-05-20T11:00:00.000Z");
  });

  test("canonicalized hydrate collisions merge cursor progress instead of using persistence order", () => {
    const persistence = inMemoryPersistence();
    persistence.saveCatchup({
      dmLastSeen: [
        ["BOB@EXAMPLE.COM/desktop", { timestamp: "2026-05-20T12:00:00.000Z", archiveId: "new" }],
        ["bob@example.com/mobile", { timestamp: "2026-05-20T10:00:00.000Z", archiveId: "old" }],
      ],
      roomLastSeen: [],
    });

    const catchup = new ReconnectCatchup(persistence);
    expect(catchup.onSessionStarted()).toEqual([{
      kind: "dm",
      key: "bob@example.com",
      scope: "account",
      after: "new",
      since: "2026-05-20T12:00:00.000Z",
    }]);
  });

  test("hydrated instance returns cursors on the *first* onSessionStarted (PR3 whole point)", () => {
    // A hydrated `ReconnectCatchup` represents a prior tab session
    // that left cursors behind in localStorage. The next page load
    // gets a fresh instance, but the very first `session:started`
    // after that load must return the hydrated cursors so MAM
    // catch-up runs immediately — otherwise the persistence is
    // dead weight (writes on every advance, never consumed).
    const persistence = inMemoryPersistence();
    persistence.saveCatchup({
      dmLastSeen: [["bob@example.com", { timestamp: "2026-05-20T10:00:00.000Z", scope: "account", archiveId: "dm-arch-1" }]],
      roomLastSeen: [],
    });

    const c = new ReconnectCatchup(persistence);
    const entries = c.onSessionStarted();
    expect(entries).toEqual([
      { kind: "dm", key: "bob@example.com", scope: "account", after: "dm-arch-1", since: "2026-05-20T10:00:00.000Z" },
    ]);
  });

  test("a fresh (un-hydrated) instance still treats the first onSessionStarted as initial login", () => {
    // Unchanged from pre-PR3 behavior: a brand-new account / first-
    // ever login has nothing to catch up on.
    const c = new ReconnectCatchup(nullResumePersistence);
    expect(c.onSessionStarted()).toEqual([]);
  });

  test("missing snapshot is a no-op (legacy / first-ever load)", () => {
    const c = new ReconnectCatchup(nullResumePersistence);
    expect(c.getDmLastSeen("bob@example.com")).toBeUndefined();
  });
});

describe("ReconnectCatchup persistence — writes back on advance", () => {
  test("recordDmSeen persists the snapshot (microtask-coalesced)", async () => {
    const persistence = inMemoryPersistence();
    const c = new ReconnectCatchup(persistence);

    c.recordDmSeen("bob@example.com", "2026-05-20T10:00:00Z", "dm-arch-1");
    expect(persistence.catchupSnapshot()).toBeNull(); // not yet written

    await flushMicrotasks();

    const snapshot = persistence.catchupSnapshot();
    expect(snapshot).not.toBeNull();
    expect(snapshot!.dmLastSeen).toContainEqual([
      "bob@example.com",
      {
        timestamp: "2026-05-20T10:00:00.000Z",
        scope: "account",
        archiveId: "dm-arch-1",
        archiveTimestamp: "2026-05-20T10:00:00.000Z",
      },
    ]);
  });

  test("persists and hydrates custom MUC occupant scope", async () => {
    const persistence = inMemoryPersistence();
    const writer = new ReconnectCatchup(persistence);
    writer.recordDmSeen(
      "room@rooms.waddle.example/alice",
      "2026-05-20T10:00:00Z",
      "pm-arch-1",
      [],
      "muc-occupant",
    );
    await flushMicrotasks();

    const reader = new ReconnectCatchup(persistence);

    expect(reader.onSessionStarted()).toEqual([{
      kind: "dm",
      key: "room@rooms.waddle.example/alice",
      scope: "muc-occupant",
      after: "pm-arch-1",
      since: "2026-05-20T10:00:00.000Z",
    }]);
  });

  test("bursts coalesce into a single write per microtask", async () => {
    const persistence = inMemoryPersistence();
    let writeCount = 0;
    const original = persistence.saveCatchup;
    persistence.saveCatchup = (snapshot) => { writeCount += 1; original(snapshot); };

    const c = new ReconnectCatchup(persistence);
    for (let i = 0; i < 10; i++) {
      c.recordDmSeen(`peer-${i}@example.com`, "2026-05-20T10:00:00Z");
    }
    await flushMicrotasks();

    expect(writeCount).toBe(1);
    expect(persistence.catchupSnapshot()!.dmLastSeen).toHaveLength(10);
  });

  test("reset clears the persisted snapshot", async () => {
    const persistence = inMemoryPersistence();
    const c = new ReconnectCatchup(persistence);
    c.recordDmSeen("bob@example.com", "2026-05-20T10:00:00Z");
    await flushMicrotasks();
    expect(persistence.catchupSnapshot()).not.toBeNull();

    c.reset();
    expect(persistence.catchupSnapshot()).toBeNull();
  });

  test("nullResumePersistence does not schedule writes (no-op fast path)", async () => {
    // The default no-op persistence should not even allocate the
    // microtask. We can't directly observe that, but we can assert
    // that no error fires and behavior is unchanged after a tick.
    const c = new ReconnectCatchup(nullResumePersistence);
    c.recordDmSeen("bob@example.com", "2026-05-20T10:00:00Z");
    await flushMicrotasks();
    expect(c.getDmLastSeen("bob@example.com")).toBe("2026-05-20T10:00:00.000Z");
  });
});

describe("applyResumeStateToWasmConfig", () => {
  function configWith(methods: string[]) {
    const calls: Array<{ method: string; args: unknown[] }> = [];
    const config: Record<string, (...args: unknown[]) => void> = {};
    for (const method of methods) {
      config[method] = (...args: unknown[]) => calls.push({ method, args });
    }
    return { config, calls };
  }

  test("uses max-aware stanza resume when both unhandled stanzas and max are available", () => {
    const { config, calls } = configWith([
      "with_resume_state_entries_with_max",
      "with_resume_state_entries",
      "with_resume_state_with_max",
      "with_resume_state",
    ]);

    applyResumeStateToWasmConfig(config, {
      previd: "prev-1",
      inboundH: 7,
      outboundH: 11,
      maxResumeSeconds: 300,
      unhandledOutboundEntries: [{ xml: "<message/>", sentAt: "2026-07-26T12:34:56.789Z" }],
    });

    expect(calls).toEqual([
      {
        method: "with_resume_state_entries_with_max",
        args: ["prev-1", 7, 11, [{ xml: "<message/>", sentAt: "2026-07-26T12:34:56.789Z" }], 300],
      },
    ]);
  });

  test("restores a pagehide fresh-stream retry tail without permitting XEP-0198 resume", () => {
    const { config, calls } = configWith(["with_fresh_stream_retry_state_entries", "with_resume_state_entries"]);

    applyResumeStateToWasmConfig(config, {
      previd: "declined-sm-stream",
      resumable: false,
      inboundH: 0,
      outboundH: 0,
      unhandledOutboundEntries: [{ xml: "<message id='m1'/>", sentAt: "2026-07-28T10:00:00.000Z" }],
    });

    expect(calls).toEqual([
      {
        method: "with_fresh_stream_retry_state_entries",
        args: ["declined-sm-stream", 0, 0, [{ xml: "<message id='m1'/>", sentAt: "2026-07-28T10:00:00.000Z" }]],
      },
    ]);
  });

  test("uses max-aware resume when no unhandled stanzas need replay", () => {
    const { config, calls } = configWith(["with_resume_state_with_max", "with_resume_state"]);

    applyResumeStateToWasmConfig(config, {
      previd: "prev-2",
      inboundH: 3,
      outboundH: 5,
      maxResumeSeconds: 120,
    });

    expect(calls).toEqual([
      {
        method: "with_resume_state_with_max",
        args: ["prev-2", 3, 5, 120],
      },
    ]);
  });

  test("uses entry resume when generated WASM lacks max-aware entry support", () => {
    const { config, calls } = configWith(["with_resume_state_entries", "with_resume_state"]);

    applyResumeStateToWasmConfig(config, {
      previd: "prev-3",
      inboundH: 1,
      outboundH: 2,
      maxResumeSeconds: 300,
      unhandledOutboundEntries: [{ xml: "<presence/>", sentAt: "2026-07-26T12:34:56.789Z" }],
    });

    expect(calls).toEqual([
      {
        method: "with_resume_state_entries",
        args: ["prev-3", 1, 2, [{ xml: "<presence/>", sentAt: "2026-07-26T12:34:56.789Z" }]],
      },
    ]);
  });

  test("does not configure a legacy resume API when sender-owned entries are present", () => {
    const { config, calls } = configWith(["with_resume_state_with_max", "with_resume_state"]);

    applyResumeStateToWasmConfig(config, {
      previd: "prev-legacy",
      inboundH: 1,
      outboundH: 2,
      maxResumeSeconds: 300,
      unhandledOutboundEntries: [{ xml: "<message id='m1'/>", sentAt: "2026-07-26T12:34:56.789Z" }],
    });

    expect(calls).toEqual([]);
  });

  test("falls back to old plain resume when generated WASM has only the legacy method", () => {
    const { config, calls } = configWith(["with_resume_state"]);

    applyResumeStateToWasmConfig(config, {
      previd: "prev-4",
      inboundH: 13,
      outboundH: 21,
      maxResumeSeconds: 300,
    });

    expect(calls).toEqual([
      {
        method: "with_resume_state",
        args: ["prev-4", 13, 21],
      },
    ]);
  });
});

describe("malformed persisted SM snapshots", () => {
  test("BrowserXmppClient discards only the SM snapshot and reconnects with a clean config", async () => {
    const persistence = inMemoryPersistence();
    persistence.saveSm({
      previd: "corrupt-sm",
      inboundH: 1,
      outboundH: 2,
      unhandledOutboundEntries: [{ xml: "<not-xml", sentAt: "not-a-timestamp" }],
    });
    let configs = 0;
    let connected = 0;
    class StubConfig {
      constructor(..._args: unknown[]) { configs += 1; }
      with_resume_state_entries() { throw new Error("invalid resume stanza XML"); }
    }
    class StubClient {
      async connect() { connected += 1; }
    }
    const client = new BrowserXmppClient(
      {
        jid: "alice@example.com",
        username: "alice",
        session_id: "token",
        xmpp_websocket_url: "wss://xmpp.example/ws",
      } as WaddleSession,
      persistence,
    );
    const state = client as unknown as {
      loadModule: () => Promise<unknown>;
      doConnect: () => Promise<void>;
    };
    state.loadModule = async () => ({ WaddleConfig: StubConfig, WaddleClient: StubClient });

    await state.doConnect();

    expect(configs).toBe(2);
    expect(connected).toBe(1);
    expect(persistence.smSnapshot()).toBeNull();
  });
});

describe("own-resume marker lifecycle (#1389)", () => {
  test.each(["resumed", "fresh"] as const)("the %s lifecycle gives the former stream the correct witness lifetime", async (lifecycle) => {
    const persistence = inMemoryPersistence();
    persistence.saveSm({ previd: `former-${lifecycle}`, inboundH: 1, outboundH: 2 });
    let lifecycleCallback: ((event: string) => void) | undefined;
    class StubConfig {
      constructor(..._args: unknown[]) {}
      with_resume_state() {}
    }
    class StubClient {
      set_on_session_lifecycle(callback: (event: string) => void) { lifecycleCallback = callback; }
      async connect() { lifecycleCallback?.(lifecycle); }
    }
    const client = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      persistence,
    );
    const state = client as unknown as {
      loadModule: () => Promise<unknown>;
      doConnect: () => Promise<void>;
    };
    state.loadModule = async () => ({ WaddleConfig: StubConfig, WaddleClient: StubClient });

    await state.doConnect();

    // A former handle can receive its conflict after the new handle's
    // `<resumed/>` callback, so only that success retains the exact witness.
    // Fresh-bind fallback still clears it immediately.
    expect(persistence.consumeOwnResume(`former-${lifecycle}`, "former-handle")).toBe(
      lifecycle === "resumed" ? "foreign" : "absent",
    );
    expect(persistence.retainedOwnResumeCount()).toBe(lifecycle === "resumed" ? 1 : 0);
  });

  test("a failed resume connect clears its witness", async () => {
    const persistence = inMemoryPersistence();
    persistence.saveSm({ previd: "former-failed", inboundH: 1, outboundH: 2 });
    class StubConfig {
      constructor(..._args: unknown[]) {}
      with_resume_state() {}
    }
    class StubClient {
      async connect() { throw new Error("resume transport failed"); }
    }
    const client = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      persistence,
    );
    const state = client as unknown as {
      loadModule: () => Promise<unknown>;
      doConnect: () => Promise<void>;
    };
    state.loadModule = async () => ({ WaddleConfig: StubConfig, WaddleClient: StubClient });

    await expect(state.doConnect()).rejects.toThrow("resume transport failed");
    expect(persistence.consumeOwnResume("former-failed", "former-handle")).toBe("absent");
  });

  test("a timed-out resume attempt clears its witness before retrying", async () => {
    const persistence = inMemoryPersistence();
    persistence.saveSm({ previd: "former-timeout", inboundH: 1, outboundH: 2 });
    class StubConfig {
      constructor(..._args: unknown[]) {}
      with_resume_state() {}
    }
    class StubClient {
      async connect() {}
      async disconnect() {}
    }
    const client = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      persistence,
    );
    const state = client as unknown as {
      loadModule: () => Promise<unknown>;
      connectTimeoutMs: number;
      reconnect: { clearTimer: () => void };
    };
    state.loadModule = async () => ({ WaddleConfig: StubConfig, WaddleClient: StubClient });
    state.connectTimeoutMs = 5;

    await expect(client.connect()).rejects.toThrow("Reconnection timed out");
    expect(persistence.consumeOwnResume("former-timeout", "former-handle")).toBe("absent");
    state.reconnect.clearTimer();
  });
});

describe("createLocalStorageResumePersistence — localStorage adapter", () => {
  test("owns an expiring exact-previd resume witness without masking a different stream", () => {
    const producer = createLocalStorageResumePersistence("alice@example.com", "resume-producer");
    const observer = createLocalStorageResumePersistence("alice@example.com", "former-stream-owner");

    producer.beginOwnResume("former-sm-stream", "resumer");
    expect(observer.consumeOwnResume("another-sm-stream", "former")).toBe("absent");
    expect(observer.consumeOwnResume("former-sm-stream", "former")).toBe("foreign");
    // A same-previd stream can be handed off multiple times before suspended
    // former documents receive their force-detach closes. Classification must
    // not let the first old client consume the latest local witness.
    expect(observer.consumeOwnResume("former-sm-stream", "another-former")).toBe("foreign");
    producer.finishOwnResume("former-sm-stream", "resumer");
    expect(observer.consumeOwnResume("former-sm-stream", "former")).toBe("absent");

    producer.beginOwnResume("fresh-fallback-stream", "resumer");
    producer.finishOwnResume("fresh-fallback-stream", "resumer");
    expect(observer.consumeOwnResume("fresh-fallback-stream", "former")).toBe("absent");

    const originalNow = Date.now;
    try {
      const createdAt = originalNow();
      Date.now = () => createdAt;
      producer.beginOwnResume("abandoned-stream", "resumer");
      const abandonedAttemptKey = readOwnResumePointer("alice@example.com", "abandoned-stream")?.attemptKey;
      Date.now = () => createdAt + 30_001;
      // Creating another marker performs the bounded abandoned-page sweep.
      producer.beginOwnResume("gc-trigger", "resumer");
      expect(window.localStorage.getItem(ownResumeKey("alice@example.com", "abandoned-stream"))).toBeNull();
      expect(abandonedAttemptKey ? window.localStorage.getItem(abandonedAttemptKey) : null).toBeNull();
      // A page that vanishes without its normal cleanup cannot leave a
      // permanent collision exemption behind.
      expect(observer.consumeOwnResume("abandoned-stream", "former")).toBe("absent");
    } finally {
      Date.now = originalNow;
    }
  });

  test("an older local client accepts a later local resume marker for the same former stream", () => {
    const first = createLocalStorageResumePersistence("alice@example.com", "first-local-owner");
    const second = createLocalStorageResumePersistence("alice@example.com", "second-local-owner");
    const third = createLocalStorageResumePersistence("alice@example.com", "third-local-owner");

    // First resumed successfully and retains its own marker while awaiting
    // the old close. Subsequent local resumptions replace that witness.
    first.beginOwnResume("same-former-stream", "first-client");
    second.beginOwnResume("same-former-stream", "second-client");
    third.beginOwnResume("same-former-stream", "third-client");

    // The first client must not treat the newer marker as its own merely
    // because it once resumed the same `previd`.
    expect(first.consumeOwnResume("same-former-stream", "first-client")).toBe("foreign");
    // Both A and B can receive the delayed conflict after C has taken over
    // this same XEP-0198 previd; neither may consume C's witness.
    expect(second.consumeOwnResume("same-former-stream", "second-client")).toBe("foreign");
  });

  test("own-resume GC compare-deletes an expired marker when another tab refreshes the same previd", () => {
    const account = "alice@example.com";
    const previd = "shared-stream";
    const key = ownResumeKey(account, previd);
    const expiredAttemptKey = ownResumeAttemptKey(
      account,
      previd,
      "old-owner",
      "old-instance",
      "old-client",
    );
    const freshAttemptKey = ownResumeAttemptKey(
      account,
      previd,
      "new-owner",
      "new-instance",
      "new-client",
    );
    const expiredMarker = JSON.stringify({
      ownerId: "old-owner",
      ownerInstanceId: "old-instance",
      clientId: "old-client",
      startedAt: Date.now() - 60_000,
      expiresAt: Date.now() - 1,
    });
    const freshMarker = JSON.stringify({
      ownerId: "new-owner",
      ownerInstanceId: "new-instance",
      clientId: "new-client",
      startedAt: Date.now(),
      expiresAt: Date.now() + 30_000,
    });
    window.localStorage.setItem(key, JSON.stringify({ attemptKey: expiredAttemptKey }));
    window.localStorage.setItem(expiredAttemptKey, expiredMarker);

    const storage = window.localStorage;
    const originalGetItem = storage.getItem.bind(storage);
    let replacementInjected = false;
    storage.getItem = ((requestedKey: string) => {
      const value = originalGetItem(requestedKey);
      if (requestedKey !== key || value !== JSON.stringify({ attemptKey: expiredAttemptKey })) return value;
      if (!replacementInjected) {
        replacementInjected = true;
        return value;
      }
      storage.setItem(key, JSON.stringify({ attemptKey: freshAttemptKey }));
      storage.setItem(freshAttemptKey, freshMarker);
      return JSON.stringify({ attemptKey: freshAttemptKey });
    }) as Storage["getItem"];

    try {
      createLocalStorageResumePersistence(account, "gc-race-owner");
      expect(window.localStorage.getItem(key)).toBe(JSON.stringify({ attemptKey: freshAttemptKey }));
      expect(window.localStorage.getItem(freshAttemptKey)).toBe(freshMarker);
    } finally {
      storage.getItem = originalGetItem;
    }
  });

  test("a delayed retain cannot replace a newer active marker for the same previd", () => {
    const account = "alice@example.com";
    const previd = "shared-stream";
    const first = createLocalStorageResumePersistence(account, "first-local-owner");
    const second = createLocalStorageResumePersistence(account, "second-local-owner");
    const observer = createLocalStorageResumePersistence(account, "observer-owner");

    first.beginOwnResume(previd, "first-client");
    const firstPointer = readOwnResumePointer(account, previd);
    expect(firstPointer).not.toBeNull();
    const firstAttemptKey = firstPointer!.attemptKey;
    const firstAttemptRaw = window.localStorage.getItem(firstAttemptKey);
    expect(firstAttemptRaw).not.toBeNull();

    const storage = window.localStorage;
    const originalGetItem = storage.getItem.bind(storage);
    let firstAttemptReads = 0;
    storage.getItem = ((requestedKey: string) => {
      const value = originalGetItem(requestedKey);
      if (requestedKey !== firstAttemptKey || value !== firstAttemptRaw) return value;
      firstAttemptReads += 1;
      if (firstAttemptReads === 2) {
        second.beginOwnResume(previd, "second-client");
      }
      return value;
    }) as Storage["getItem"];

    try {
      first.retainOwnResume(previd, "first-client");
    } finally {
      storage.getItem = originalGetItem;
    }

    const activePointer = readOwnResumePointer(account, previd);
    expect(activePointer).not.toBeNull();
    expect(activePointer?.attemptKey).not.toBe(firstAttemptKey);
    expect(observer.consumeOwnResume(previd, "former-client")).toBe("foreign");
    expect(first.consumeOwnResume(previd, "first-client")).toBe("foreign");
    expect(second.consumeOwnResume(previd, "second-client")).toBe("self");
  });

  test("a failed retry restores the prior retained witness for the same client", () => {
    const account = "alice@example.com";
    const previd = "shared-stream";
    const producer = createLocalStorageResumePersistence(account, "resume-producer");
    const observer = createLocalStorageResumePersistence(account, "former-stream-owner");

    producer.beginOwnResume(previd, "resumer");
    producer.retainOwnResume(previd, "resumer");
    const firstPointer = readOwnResumePointer(account, previd);
    expect(firstPointer).not.toBeNull();

    producer.beginOwnResume(previd, "resumer");
    const retryPointer = readOwnResumePointer(account, previd);
    expect(retryPointer).not.toBeNull();
    expect(retryPointer?.attemptKey).not.toBe(firstPointer?.attemptKey);

    producer.finishOwnResume(previd, "resumer");

    expect(readOwnResumePointer(account, previd)?.attemptKey).toBe(firstPointer?.attemptKey);
    expect(observer.consumeOwnResume(previd, "former-client")).toBe("foreign");
  });

  test("a failed retry's fallback restore never overwrites a newer tab's pointer", () => {
    const account = "alice@example.com";
    const previd = "shared-stream";
    const producer = createLocalStorageResumePersistence(account, "resume-producer");
    const competitor = createLocalStorageResumePersistence(account, "resume-competitor");

    producer.beginOwnResume(previd, "resumer", "attempt-1");
    producer.retainOwnResume(previd, "resumer", "attempt-1");
    producer.beginOwnResume(previd, "resumer", "attempt-2");

    // Interleave tab C's newer attempt while the finisher scans fallback
    // attempts: the scan reads attempt keys other than the one being
    // removed, which is exactly the window between the pointer currency
    // check and the fallback write (codex 1668 round).
    const ls = window.localStorage;
    const originalGetItem = ls.getItem.bind(ls);
    let injected = false;
    let competitorPointer: { attemptKey: string } | null = null;
    ls.getItem = (key: string) => {
      const value = originalGetItem(key);
      if (!injected && key.includes(".attempt.") && !key.includes("attempt-2")) {
        injected = true;
        competitor.beginOwnResume(previd, "competitor-client", "attempt-C");
        competitorPointer = readOwnResumePointer(account, previd);
      }
      return value;
    };
    try {
      producer.finishOwnResume(previd, "resumer", "attempt-2");
    } finally {
      ls.getItem = originalGetItem;
    }

    expect(injected).toBe(true);
    expect(competitorPointer).not.toBeNull();
    expect(readOwnResumePointer(account, previd)?.attemptKey).toBe(
      competitorPointer!.attemptKey,
    );
    // The displaced counterpart of C's attempt still classifies as foreign
    // (superseded recovery), not as its own terminal marker.
    expect(producer.consumeOwnResume(previd, "resumer")).toBe("foreign");
  });

  test("same-timestamp retries remove only the exact explicit attempt marker", () => {
    const account = "alice@example.com";
    const previd = "shared-stream";
    const producer = createLocalStorageResumePersistence(account, "resume-producer");
    const observer = createLocalStorageResumePersistence(account, "former-stream-owner");
    const originalNow = Date.now;

    try {
      Date.now = () => 1_700_000_000_000;
      producer.beginOwnResume(previd, "resumer", "attempt-1");
      producer.retainOwnResume(previd, "resumer", "attempt-1");
      const firstPointer = readOwnResumePointer(account, previd);
      expect(firstPointer).not.toBeNull();

      producer.beginOwnResume(previd, "resumer", "attempt-2");
      const retryPointer = readOwnResumePointer(account, previd);
      expect(retryPointer).not.toBeNull();
      expect(retryPointer?.attemptKey).not.toBe(firstPointer?.attemptKey);

      producer.finishOwnResume(previd, "resumer", "attempt-2");

      expect(readOwnResumePointer(account, previd)?.attemptKey).toBe(firstPointer?.attemptKey);
      expect(firstPointer ? window.localStorage.getItem(firstPointer.attemptKey) : null).not.toBeNull();
      expect(retryPointer ? window.localStorage.getItem(retryPointer.attemptKey) : null).toBeNull();
      expect(observer.consumeOwnResume(previd, "former-client")).toBe("foreign");
    } finally {
      Date.now = originalNow;
    }
  });

  test("refreshes a successful resume witness beyond the initial attempt TTL", () => {
    const producer = createLocalStorageResumePersistence("alice@example.com", "resume-producer");
    const observer = createLocalStorageResumePersistence("alice@example.com", "former-stream-owner");
    const originalNow = Date.now;
    try {
      const startedAt = originalNow();
      Date.now = () => startedAt;
      producer.beginOwnResume("former-sm-stream", "resumer");

      // A resume success shortly before the original 30s attempt deadline
      // keeps the former document's force-detach witness alive for 5 minutes.
      Date.now = () => startedAt + 29_999;
      producer.retainOwnResume("former-sm-stream", "resumer");
      Date.now = () => startedAt + 30_001;
      expect(observer.consumeOwnResume("former-sm-stream", "former")).toBe("foreign");

      Date.now = () => startedAt + 330_000;
      expect(observer.consumeOwnResume("former-sm-stream", "former")).toBe("absent");
    } finally {
      Date.now = originalNow;
    }
  });

  test("ref-counts owner heartbeats and drains all live ownership on terminal disposal", () => {
    const timers = ownerTimerHarness();
    const baseline = resumeOwnerLifecycleSnapshotForTests();
    const first = createLocalStorageResumePersistence("alice@example.com", undefined, {
      ownerTimerDriver: timers.driver,
    });
    const second = createLocalStorageResumePersistence("alice@example.com", undefined, {
      ownerTimerDriver: timers.driver,
    });

    expect(timers.activeCount()).toBe(1);
    expect(resumeOwnerLifecycleSnapshotForTests()).toEqual({
      registrations: baseline.registrations + 1,
      activeTimers: baseline.activeTimers + 1,
      ownerInstances: baseline.ownerInstances + 1,
    });

    first.dispose();
    expect(timers.activeCount()).toBe(1);
    expect(resumeOwnerLifecycleSnapshotForTests().registrations).toBe(baseline.registrations + 1);

    second.dispose();
    expect(timers.activeCount()).toBe(0);
    expect(resumeOwnerLifecycleSnapshotForTests()).toEqual(baseline);
  });

  test("a throwing owner scheduler rolls back only its provisional lease and permits reacquisition", () => {
    const baseline = resumeOwnerLifecycleSnapshotForTests();
    const throwingDriver = {
      setInterval: () => { throw new Error("scheduler unavailable"); },
      clearInterval: () => undefined,
    };

    expect(() => createLocalStorageResumePersistence("alice@example.com", undefined, {
      ownerTimerDriver: throwingDriver,
    })).toThrow("scheduler unavailable");
    expect(resumeOwnerLifecycleSnapshotForTests()).toEqual(baseline);

    const timers = ownerTimerHarness();
    const recovered = createLocalStorageResumePersistence("alice@example.com", undefined, {
      ownerTimerDriver: timers.driver,
    });
    expect(timers.activeCount()).toBe(1);
    expect(resumeOwnerLifecycleSnapshotForTests()).toEqual({
      registrations: baseline.registrations + 1,
      activeTimers: baseline.activeTimers + 1,
      ownerInstances: baseline.ownerInstances + 1,
    });
    recovered.dispose();
    expect(resumeOwnerLifecycleSnapshotForTests()).toEqual(baseline);
  });

  test("a re-entrant throwing scheduler leaves its live successor's owner lease intact", () => {
    const baseline = resumeOwnerLifecycleSnapshotForTests();
    const timers = ownerTimerHarness();
    let successor: ResumePersistence | null = null;
    const reentrantThrowingDriver = {
      setInterval: () => {
        successor = createLocalStorageResumePersistence("alice@example.com", undefined, {
          ownerTimerDriver: timers.driver,
        });
        throw new Error("outer scheduler unavailable");
      },
      clearInterval: () => undefined,
    };

    expect(() => createLocalStorageResumePersistence("alice@example.com", undefined, {
      ownerTimerDriver: reentrantThrowingDriver,
    })).toThrow("outer scheduler unavailable");
    if (!successor) throw new Error("re-entrant scheduler did not retain a successor");

    const ownerId = window.sessionStorage.getItem("waddle.chat.sm-resume.owner");
    expect(ownerId).not.toBeNull();
    const leaseKey = ownerLeaseKey("alice@example.com", ownerId!);
    const successorLease = window.localStorage.getItem(leaseKey);
    expect(successorLease).not.toBeNull();
    expect(timers.activeCount()).toBe(1);
    expect(resumeOwnerLifecycleSnapshotForTests()).toEqual({
      registrations: baseline.registrations + 1,
      activeTimers: baseline.activeTimers + 1,
      ownerInstances: baseline.ownerInstances + 1,
    });

    const sharedSuccessor = createLocalStorageResumePersistence("alice@example.com", undefined, {
      ownerTimerDriver: timers.driver,
    });
    successor.dispose();
    expect(timers.activeCount()).toBe(1);
    expect(window.sessionStorage.getItem("waddle.chat.sm-resume.owner")).toBe(ownerId);
    expect(window.localStorage.getItem(leaseKey)).toBe(successorLease);

    sharedSuccessor.dispose();
    expect(timers.activeCount()).toBe(0);
    expect(resumeOwnerLifecycleSnapshotForTests()).toEqual(baseline);

    const reacquired = createLocalStorageResumePersistence("alice@example.com", undefined, {
      ownerTimerDriver: timers.driver,
    });
    expect(timers.activeCount()).toBe(1);
    reacquired.dispose();
    expect(resumeOwnerLifecycleSnapshotForTests()).toEqual(baseline);
  });

  test("a failed nested scheduler cannot suppress its still-scheduling parent", () => {
    const baseline = resumeOwnerLifecycleSnapshotForTests();
    const timers = ownerTimerHarness();
    let nested = false;
    const innerFailure = {
      setInterval: () => { throw new Error("inner scheduler unavailable"); },
      clearInterval: () => undefined,
    };
    const outerDriver = {
      setInterval: (callback: () => void) => {
        if (!nested) {
          nested = true;
          expect(() => createLocalStorageResumePersistence("alice@example.com", undefined, {
            ownerTimerDriver: innerFailure,
          })).toThrow("inner scheduler unavailable");
        }
        return timers.driver.setInterval(callback, 0);
      },
      clearInterval: timers.driver.clearInterval,
    };

    const outer = createLocalStorageResumePersistence("alice@example.com", undefined, {
      ownerTimerDriver: outerDriver,
    });
    const ownerId = window.sessionStorage.getItem("waddle.chat.sm-resume.owner");
    expect(ownerId).not.toBeNull();
    const leaseKey = ownerLeaseKey("alice@example.com", ownerId!);
    expect(window.localStorage.getItem(leaseKey)).not.toBeNull();
    expect(timers.activeCount()).toBe(1);
    expect(resumeOwnerLifecycleSnapshotForTests()).toEqual({
      registrations: baseline.registrations + 1,
      activeTimers: baseline.activeTimers + 1,
      ownerInstances: baseline.ownerInstances + 1,
    });

    // The outer registration is real ownership, not a discarded stale
    // closure: sharing, terminal release, and a fresh acquisition all work.
    const shared = createLocalStorageResumePersistence("alice@example.com", undefined, {
      ownerTimerDriver: timers.driver,
    });
    outer.dispose();
    expect(timers.activeCount()).toBe(1);
    expect(window.localStorage.getItem(leaseKey)).not.toBeNull();
    shared.dispose();
    expect(timers.activeCount()).toBe(0);
    expect(resumeOwnerLifecycleSnapshotForTests()).toEqual(baseline);

    const reacquired = createLocalStorageResumePersistence("alice@example.com", undefined, {
      ownerTimerDriver: timers.driver,
    });
    expect(timers.activeCount()).toBe(1);
    reacquired.dispose();
    expect(resumeOwnerLifecycleSnapshotForTests()).toEqual(baseline);
  });

  test("a disposed owner cannot mutate persistence, even if a captured heartbeat ticks", () => {
    const timers = ownerTimerHarness();
    const persistence = createLocalStorageResumePersistence("alice@example.com", undefined, {
      ownerTimerDriver: timers.driver,
    });
    const capturedTick = timers.callbacks()[0];
    persistence.saveSm({ previd: "tail", inboundH: 1, outboundH: 2 });
    persistence.dispose();
    const afterDispose = [...Array(window.localStorage.length)].map((_, index) => [
      window.localStorage.key(index),
      window.localStorage.getItem(window.localStorage.key(index) ?? ""),
    ]);

    capturedTick?.();
    persistence.saveSm({ previd: "new-tail", inboundH: 3, outboundH: 4 });
    persistence.saveCatchup({ dmLastSeen: [], roomLastSeen: [] });
    persistence.clearSm();
    persistence.clearCatchup();
    persistence.clearJoinedRooms();
    persistence.preparePagehideHandoff();

    expect(persistence.loadSm()).toBeNull();
    expect(persistence.consumeSm()).toBeNull();
    expect(persistence.loadCatchup()).toBeNull();
    expect(persistence.loadJoinedRooms()).toEqual([]);
    expect([...Array(window.localStorage.length)].map((_, index) => [
      window.localStorage.key(index),
      window.localStorage.getItem(window.localStorage.key(index) ?? ""),
    ])).toEqual(afterDispose);
  });

  test("a stale disposed owner cannot remove a replacement lease, handoff, or SM tail", () => {
    const firstTimers = ownerTimerHarness();
    const first = createLocalStorageResumePersistence("alice@example.com", undefined, {
      ownerTimerDriver: firstTimers.driver,
    });
    const ownerId = window.sessionStorage.getItem("waddle.chat.sm-resume.owner")!;
    const leaseKey = ownerLeaseKey("alice@example.com", ownerId);
    const handoffKey = ownerHandoffKey("alice@example.com", ownerId);
    const tailKey = smOwnerKey("alice@example.com", ownerId);
    const replacementInstanceId = "replacement-generation";
    window.localStorage.setItem(leaseKey, JSON.stringify({
      ownerId,
      instanceId: replacementInstanceId,
      updatedAt: Date.now(),
    }));
    window.localStorage.setItem(handoffKey, JSON.stringify({
      ownerId,
      instanceId: replacementInstanceId,
      expiresAt: Date.now() + 45_000,
    }));
    window.localStorage.setItem(tailKey, JSON.stringify({
      previd: "replacement-tail",
      inboundH: 5,
      outboundH: 6,
      savedAt: Date.now(),
      ownerId,
      ownerInstanceId: replacementInstanceId,
    }));
    const expectedLease = window.localStorage.getItem(leaseKey);
    const expectedHandoff = window.localStorage.getItem(handoffKey);
    const expectedTail = window.localStorage.getItem(tailKey);

    first.dispose();

    expect(window.localStorage.getItem(leaseKey)).toBe(expectedLease);
    expect(window.localStorage.getItem(handoffKey)).toBe(expectedHandoff);
    expect(window.localStorage.getItem(tailKey)).toBe(expectedTail);
  });

  test("only terminal BrowserXmppClient disposal releases the persistence owner exactly once", async () => {
    const persistence = inMemoryPersistence();
    let disposeCalls = 0;
    persistence.dispose = () => { disposeCalls += 1; };
    const client = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      persistence,
    );

    await client.disconnect();
    expect(disposeCalls).toBe(0);
    await client.dispose();
    await client.dispose();

    expect(disposeCalls).toBe(1);
    await expect(client.connect()).rejects.toThrow("XMPP client is disposed");
    await expect(client.sendDirectMessage("bob@example.com", "after disposal")).rejects.toThrow("XMPP client is disposed");
  });

  test("terminal disposal releases ownership before a stalled socket can repersist pagehide state", async () => {
    const timers = ownerTimerHarness();
    const persistence = createLocalStorageResumePersistence("alice@example.com", undefined, {
      ownerTimerDriver: timers.driver,
    });
    const client = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      persistence,
    );
    const socketClose = deferred<void>();
    let acknowledgementRequests = 0;
    (client as unknown as {
      xmpp: {
        disconnect: () => Promise<void>;
        get_resume_state: () => PersistedSmResumeState;
        try_request_stream_management_ack_for_pagehide: () => "accepted";
      };
    }).xmpp = {
      disconnect: () => socketClose.promise,
      get_resume_state: () => ({ previd: "must-not-persist", inboundH: 1, outboundH: 1 }),
      try_request_stream_management_ack_for_pagehide: () => {
        acknowledgementRequests += 1;
        return "accepted";
      },
    };

    const disposing = client.dispose();
    client.prepareForPageHide();

    expect(timers.activeCount()).toBe(0);
    expect(acknowledgementRequests).toBe(0);
    expect(persistence.loadSm()).toBeNull();

    socketClose.resolve();
    await disposing;
  });

  test("round-trips a catchup snapshot through localStorage", () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com");
    const snapshot: PersistedReconnectCatchup = {
      dmLastSeen: [["bob@example.com", { timestamp: "2026-05-20T10:00:00.000Z" }]],
      roomLastSeen: [],
    };
    persistence.saveCatchup(snapshot);
    expect(persistence.loadCatchup()).toEqual(snapshot);
  });

  test("round-trips an SM resume state through localStorage (with internal savedAt)", () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com");
    const state = {
      previd: "abc-123",
      inboundH: 42,
      outboundH: 7,
      unhandledOutboundEntries: [{ xml: "<message xmlns='jabber:client' id='m1'/>", sentAt: "2026-07-26T12:34:56.789Z" }],
    };
    persistence.saveSm(state);
    // Round-trip strips the internal `savedAt` so the caller gets
    // the same shape it passed in.
    expect(persistence.loadSm()).toEqual(state);
  });

  test("localStorage consume preserves a nonresumable fresh-stream tail for config application", () => {
    const owner = "fresh-stream-owner";
    const writer = createLocalStorageResumePersistence("alice@example.com", owner);
    const state: PersistedSmResumeState = {
      previd: "declined-sm-stream",
      resumable: false,
      inboundH: 0,
      outboundH: 0,
      unhandledOutboundEntries: [
        {
          xml: "<message xmlns='jabber:client' id='message-retry'><origin-id xmlns='urn:xmpp:sid:0' id='message-origin'/><delay xmlns='urn:xmpp:delay' stamp='2026-07-28T10:11:12.000Z'/></message>",
          sentAt: "2026-07-28T10:11:12.000Z",
        },
        {
          xml: "<presence xmlns='jabber:client' id='presence-retry'><delay xmlns='urn:xmpp:delay' stamp='2026-07-28T10:11:12.000Z'/></presence>",
          sentAt: "2026-07-28T10:11:12.000Z",
        },
      ],
    };
    writer.saveSm(state);
    const reader = createLocalStorageResumePersistence("alice@example.com", owner);
    const calls: Array<{ method: string; args: unknown[] }> = [];
    const config = {
      with_fresh_stream_retry_state_entries: (...args: unknown[]) => {
        calls.push({ method: "fresh-stream", args });
      },
      with_resume_state_entries: (...args: unknown[]) => {
        calls.push({ method: "resume", args });
      },
    };

    try {
      expect(reader.loadSm()).toEqual(state);
      const consumed = reader.consumeSm();
      expect(consumed).toEqual(state);
      applyResumeStateToWasmConfig(config, consumed!);
      expect(calls).toEqual([{
        method: "fresh-stream",
        args: [state.previd, state.inboundH, state.outboundH, state.unhandledOutboundEntries],
      }]);
      expect(reader.consumeSm()).toBeNull();
    } finally {
      reader.dispose();
      writer.dispose();
    }
  });

  test("fails closed for a legacy nonempty stanza snapshot", () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com", "legacy-owner");
    window.localStorage.setItem(smOwnerKey("alice@example.com", "legacy-owner"), JSON.stringify({
      previd: "legacy",
      inboundH: 1,
      outboundH: 2,
      savedAt: Date.now(),
      ownerId: "unowned",
      unhandledOutboundStanzas: ["<message id='m1'/>"] ,
    }));

    expect(persistence.loadSm()).toBeNull();
  });

  test("round-trips the bound resource with SM resume state", () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com");
    const state = {
      previd: "abc-123",
      inboundH: 42,
      outboundH: 7,
      resource: "web-existing-resource",
    };
    persistence.saveSm(state);
    expect(persistence.loadSm()).toEqual(state);
  });

  test("consumeSm claims and clears the stored resource for only one client", () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com");
    const state = {
      previd: "abc-123",
      inboundH: 42,
      outboundH: 7,
      resource: "web-existing-resource",
    };
    persistence.saveSm(state);
    expect(persistence.consumeSm()).toEqual(state);
    expect(persistence.consumeSm()).toBeNull();
    expect(persistence.loadSm()).toBeNull();
  });

  test("consumeSm is scoped to the tab owner", () => {
    const tabA = createLocalStorageResumePersistence("alice@example.com", "tab-a");
    const tabB = createLocalStorageResumePersistence("alice@example.com", "tab-b");
    const state = {
      previd: "abc-123",
      inboundH: 42,
      outboundH: 7,
      resource: "web-existing-resource",
    };
    tabA.saveSm(state);

    expect(tabB.loadSm()).toBeNull();
    expect(tabB.consumeSm()).toBeNull();
    expect(tabA.consumeSm()).toEqual(state);
    expect(tabA.consumeSm()).toBeNull();
  });

  test("a duplicate tab's lifecycle writes cannot replace the live owner's reload handoff", () => {
    const account = "alice@example.com";
    const ownerA = "handoff-owner-a";
    const ownerB = "duplicate-owner-b";
    const aState = { previd: "resume-a", inboundH: 11, outboundH: 12, resource: "web-a" };
    const bState = { previd: "resume-b", inboundH: 21, outboundH: 22, resource: "web-b" };
    createLocalStorageResumePersistence(account, ownerA).saveSm(aState);
    window.sessionStorage.setItem("waddle.chat.sm-resume.owner", ownerA);
    window.localStorage.setItem(
      ownerLeaseKey(account, ownerA),
      JSON.stringify({ ownerId: ownerA, instanceId: "before-reload", updatedAt: Date.now() }),
    );
    window.localStorage.setItem(
      ownerHandoffKey(account, ownerA),
      JSON.stringify({ ownerId: ownerA, instanceId: "before-reload", expiresAt: Date.now() + 45_000 }),
    );

    const duplicate = createLocalStorageResumePersistence(account, ownerB);
    duplicate.saveSm(bState);
    duplicate.preparePagehideHandoff();
    duplicate.clearSm();

    const restoreNavigation = installNavigationTiming("reload");
    try {
      const reloadedA = createLocalStorageResumePersistence(account);
      expect(reloadedA.loadSm()).toEqual(aState);
      expect(reloadedA.consumeSm()).toEqual(aState);
      expect(reloadedA.consumeSm()).toBeNull();
      expect(createLocalStorageResumePersistence(account, ownerB).loadSm()).toBeNull();
    } finally {
      restoreNavigation();
    }
  });

  test("concurrent owner clears and consumed markers stay isolated", () => {
    const account = "alice@example.com";
    const ownerA = createLocalStorageResumePersistence(account, "clear-owner-a");
    const ownerB = createLocalStorageResumePersistence(account, "clear-owner-b");
    const aState = { previd: "clear-a", inboundH: 1, outboundH: 2 };
    const bState = { previd: "clear-b", inboundH: 3, outboundH: 4 };
    ownerA.saveSm(aState);
    ownerB.saveSm(bState);

    ownerA.clearSm();
    expect(ownerA.loadSm()).toBeNull();
    expect(ownerB.loadSm()).toEqual(bState);
    expect(ownerB.consumeSm()).toEqual(bState);
    expect(window.localStorage.getItem(smConsumedKey(account, "clear-owner-a"))).toBeNull();
    expect(window.localStorage.getItem(smConsumedKey(account, "clear-owner-b"))).not.toBeNull();
  });

  test("a damaged consume marker fails closed without exposing the replay tail", () => {
    const account = "alice@example.com";
    const owner = "damaged-marker-owner";
    const persistence = createLocalStorageResumePersistence(account, owner);
    persistence.saveSm({ previd: "must-not-resume", inboundH: 1, outboundH: 2, resource: "web-owned" });
    window.localStorage.setItem(smConsumedKey(account, owner), "{damaged");

    expect(persistence.loadSm()).toBeNull();
    expect(persistence.consumeSm()).toBeNull();
    expect(window.localStorage.getItem(smOwnerKey(account, owner))).toBeNull();
    expect(window.localStorage.getItem(smConsumedKey(account, owner))).toBeNull();
  });

  test("a mismatched consume marker and snapshot fail closed on direct consume", () => {
    const account = "alice@example.com";
    const owner = "mismatched-marker-owner";
    const persistence = createLocalStorageResumePersistence(account, owner);
    persistence.saveSm({ previd: "must-not-resume", inboundH: 1, outboundH: 2 });
    window.localStorage.setItem(
      smConsumedKey(account, owner),
      JSON.stringify({ marker: "different-snapshot", savedAt: Date.now() }),
    );

    expect(persistence.consumeSm()).toBeNull();
    expect(window.localStorage.getItem(smOwnerKey(account, owner))).toBeNull();
    expect(window.localStorage.getItem(smConsumedKey(account, owner))).toBeNull();
  });

  test("owner-slot GC removes expired tails but never a live owner tail", () => {
    const account = "alice@example.com";
    const active = createLocalStorageResumePersistence(account, "active-owner");
    const staleOwner = "stale-owner";
    const activeState = { previd: "active", inboundH: 5, outboundH: 6 };
    active.saveSm(activeState);
    window.localStorage.setItem(
      smOwnerKey(account, staleOwner),
      JSON.stringify({
        previd: "stale",
        inboundH: 7,
        outboundH: 8,
        ownerId: staleOwner,
        savedAt: Date.now() - 301_000,
      }),
    );
    window.localStorage.setItem(
      smConsumedKey(account, staleOwner),
      JSON.stringify({ marker: "stale", savedAt: Date.now() - 301_000 }),
    );

    createLocalStorageResumePersistence(account, "gc-trigger-owner").saveSm({ previd: "trigger", inboundH: 9, outboundH: 10 });

    expect(active.loadSm()).toEqual(activeState);
    expect(window.localStorage.getItem(smOwnerKey(account, staleOwner))).toBeNull();
    expect(window.localStorage.getItem(smConsumedKey(account, staleOwner))).toBeNull();
  });

  test("a full owner window preserves existing tails and fails closed for a new owner", () => {
    const account = "alice@example.com";
    const current = createLocalStorageResumePersistence(account, "bounded-current");
    current.saveSm({ previd: "before-update", inboundH: 1, outboundH: 2 });
    for (let index = 1; index < 64; index += 1) {
      createLocalStorageResumePersistence(account, `bounded-${index}`).saveSm({
        previd: `tail-${index}`,
        inboundH: index,
        outboundH: index,
      });
    }

    current.saveSm({ previd: "after-update", inboundH: 3, outboundH: 4 });
    const overflow = createLocalStorageResumePersistence(account, "bounded-overflow");
    overflow.saveSm({ previd: "must-not-persist", inboundH: 5, outboundH: 6 });

    expect(current.loadSm()).toEqual({ previd: "after-update", inboundH: 3, outboundH: 4 });
    expect(overflow.loadSm()).toBeNull();
    expect(window.localStorage.length).toBe(64);
  });

  test("consumed owner markers share the bounded owner window and preserve live tails", () => {
    const account = "alice@example.com";
    const live = createLocalStorageResumePersistence(account, "live-owner");
    const liveState = { previd: "live-stream", inboundH: 1, outboundH: 2 };
    live.saveSm(liveState);

    for (let index = 1; index < 64; index += 1) {
      const consumed = createLocalStorageResumePersistence(account, `consumed-${index}`);
      const state = { previd: `consumed-stream-${index}`, inboundH: index, outboundH: index };
      consumed.saveSm(state);
      expect(consumed.consumeSm()).toEqual(state);
    }

    const overflow = createLocalStorageResumePersistence(account, "consumed-overflow");
    overflow.saveSm({ previd: "must-not-persist", inboundH: 5, outboundH: 6 });

    expect(live.loadSm()).toEqual(liveState);
    expect(overflow.loadSm()).toBeNull();
    expect(window.localStorage.length).toBe(64);
  });

  test("GC removes noncanonical owner keys before they can consume the owner window", () => {
    const account = "alice@example.com";
    const corruptKey = `waddle.chat.sm-resume.${account.length}:${account}.01:x`;
    window.localStorage.setItem(
      corruptKey,
      JSON.stringify({
        previd: "corrupt-stream",
        inboundH: 1,
        outboundH: 2,
        ownerId: "x",
        savedAt: Date.now(),
      }),
    );

    const valid = createLocalStorageResumePersistence(account, "valid-owner");
    const validState = { previd: "valid-stream", inboundH: 3, outboundH: 4 };
    valid.saveSm(validState);

    expect(window.localStorage.getItem(corruptKey)).toBeNull();
    expect(valid.loadSm()).toEqual(validState);
  });

  test("GC drops interrupted claims so they cannot exhaust the owner window", () => {
    const account = "alice@example.com";
    for (let index = 0; index < 64; index += 1) {
      const ownerId = `interrupted-${index}`;
      window.localStorage.setItem(
        smOwnerKey(account, ownerId),
        JSON.stringify({
          previd: `interrupted-stream-${index}`,
          inboundH: index,
          outboundH: index,
          ownerId,
          claimId: `claim-${index}`,
          savedAt: Date.now(),
        }),
      );
    }

    const recovered = createLocalStorageResumePersistence(account, "recovered-owner");
    recovered.saveSm({ previd: "recovered-stream", inboundH: 1, outboundH: 2 });

    expect(recovered.loadSm()).toEqual({ previd: "recovered-stream", inboundH: 1, outboundH: 2 });
    expect(window.localStorage.length).toBe(1);
  });

  test("an owner ID ending in consumed retains its snapshot and marker boundary", () => {
    const account = "alice@example.com";
    const owner = "tab.consumed";
    const persistence = createLocalStorageResumePersistence(account, owner);
    const state = { previd: "suffix-owner", inboundH: 1, outboundH: 2 };
    persistence.saveSm(state);

    expect(persistence.loadSm()).toEqual(state);
    expect(persistence.consumeSm()).toEqual(state);
    expect(window.localStorage.getItem(smOwnerKey(account, owner))).toBeNull();
    expect(window.localStorage.getItem(smConsumedKey(account, owner))).not.toBeNull();
  });

  test("a duplicated tab rotates a copied live owner before it can claim SM state", () => {
    const copiedOwner = "copied-live-owner";
    const state = {
      previd: "abc-123",
      inboundH: 42,
      outboundH: 7,
      resource: "web-existing-resource",
    };
    createLocalStorageResumePersistence("alice@example.com", copiedOwner).saveSm(state);
    window.sessionStorage.setItem("waddle.chat.sm-resume.owner", copiedOwner);
    window.localStorage.setItem(
      ownerLeaseKey("alice@example.com", copiedOwner),
      JSON.stringify({
        ownerId: copiedOwner,
        instanceId: "original-live-tab",
        updatedAt: Date.now(),
      }),
    );

    const duplicatedTab = createLocalStorageResumePersistence("alice@example.com");

    expect(duplicatedTab.consumeSm()).toBeNull();
    expect(createLocalStorageResumePersistence("alice@example.com", copiedOwner).consumeSm()).toEqual(state);
  });

  test("a matching pagehide handoff keeps the owner only for a confirmed same-tab reload", () => {
    const reloadOwner = "reload-owner";
    const state = {
      previd: "abc-123",
      inboundH: 42,
      outboundH: 7,
      resource: "web-existing-resource",
    };
    seedCopiedOwnerHandoff(reloadOwner, state);
    const restoreNavigation = installNavigationTiming("reload");
    try {
      const reloadedPage = createLocalStorageResumePersistence("alice@example.com");

      expect(reloadedPage.loadSm()).toEqual(state);
      expect(reloadedPage.consumeSm()).toEqual(state);
    } finally {
      restoreNavigation();
    }
  });

  test("a navigate clone with a valid handoff rotates and cannot load or consume SM state", () => {
    const ownerId = "navigate-clone-owner";
    const state = {
      previd: "abc-123",
      inboundH: 42,
      outboundH: 7,
      resource: "web-existing-resource",
    };
    seedCopiedOwnerHandoff(ownerId, state);
    const restoreNavigation = installNavigationTiming("navigate");
    try {
      const duplicate = createLocalStorageResumePersistence("alice@example.com");

      expect(duplicate.loadSm()).toBeNull();
      expect(duplicate.consumeSm()).toBeNull();
    } finally {
      restoreNavigation();
    }
  });

  test("unknown or unavailable navigation timing fails closed", () => {
    for (const type of ["unknown", null]) {
      const ownerId = `closed-navigation-${type ?? "unavailable"}`;
      const state = { previd: "abc-123", inboundH: 42, outboundH: 7 };
      seedCopiedOwnerHandoff(ownerId, state);
      const restoreNavigation = installNavigationTiming(type);
      try {
        const duplicate = createLocalStorageResumePersistence("alice@example.com");
        expect(duplicate.loadSm()).toBeNull();
        expect(duplicate.consumeSm()).toBeNull();
      } finally {
        restoreNavigation();
      }
    }
  });

  test("history, prerender, and unknown copied-owner navigations never consume the handoff", () => {
    for (const type of ["navigate", "back_forward", "prerender", "unknown", null]) {
      const ownerId = `copied-owner-${type ?? "unavailable"}`;
      const state = { previd: "abc-123", inboundH: 42, outboundH: 7 };
      seedCopiedOwnerHandoff(ownerId, state);
      const restoreNavigation = installNavigationTiming(type);
      try {
        const copiedDocument = createLocalStorageResumePersistence("alice@example.com");
        expect(copiedDocument.loadSm()).toBeNull();
        expect(copiedDocument.consumeSm()).toBeNull();
        expect(createLocalStorageResumePersistence("alice@example.com", ownerId).consumeSm()).toEqual(state);
      } finally {
        restoreNavigation();
      }
    }
  });

  test("reload rotates copied ownership without a current matching lease", () => {
    for (const scenario of ["missing", "expired", "mismatched"] as const) {
      const ownerId = `reload-${scenario}-lease-owner`;
      const state = { previd: "abc-123", inboundH: 42, outboundH: 7 };
      seedCopiedOwnerHandoff(ownerId, state);
      const leaseKey = ownerLeaseKey("alice@example.com", ownerId);
      if (scenario === "missing") {
        window.localStorage.removeItem(leaseKey);
      } else if (scenario === "expired") {
        window.localStorage.setItem(
          leaseKey,
          JSON.stringify({ ownerId, instanceId: "previous-page", updatedAt: Date.now() - 45_001 }),
        );
      } else {
        window.localStorage.setItem(
          leaseKey,
          JSON.stringify({ ownerId: "another-owner", instanceId: "previous-page", updatedAt: Date.now() }),
        );
      }

      const restoreNavigation = installNavigationTiming("reload");
      try {
        const copiedDocument = createLocalStorageResumePersistence("alice@example.com");
        expect(copiedDocument.loadSm()).toBeNull();
        expect(copiedDocument.consumeSm()).toBeNull();
        expect(createLocalStorageResumePersistence("alice@example.com", ownerId).consumeSm()).toEqual(state);
      } finally {
        restoreNavigation();
      }
    }
  });

  test("a reload rejects a handoff that does not belong to the active lease", () => {
    const ownerId = "mismatched-handoff-owner";
    const state = { previd: "abc-123", inboundH: 42, outboundH: 7 };
    seedCopiedOwnerHandoff(ownerId, state, "other-page");
    const restoreNavigation = installNavigationTiming("reload");
    try {
      const duplicate = createLocalStorageResumePersistence("alice@example.com");
      expect(duplicate.loadSm()).toBeNull();
      expect(duplicate.consumeSm()).toBeNull();
    } finally {
      restoreNavigation();
    }
  });

  test("BrowserXmppClient only reuses a refreshed resource for the owning tab", () => {
    const state = {
      previd: "abc-123",
      inboundH: 42,
      outboundH: 7,
      resource: "web-existing-resource",
    };
    createLocalStorageResumePersistence("alice@example.com", "tab-a").saveSm(state);

    const tabB = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      createLocalStorageResumePersistence("alice@example.com", "tab-b"),
    );
    expect(tabB.fullJid).not.toBe("alice@example.com/web-existing-resource");

    const tabA = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      createLocalStorageResumePersistence("alice@example.com", "tab-a"),
    );
    expect(tabA.fullJid).toBe("alice@example.com/web-existing-resource");
  });

  test("explicit superseded recovery binds a fresh resource without replaying a successor-owned SM tail", async () => {
    const persistence = inMemoryPersistence();
    persistence.saveSm({
      previd: "former-stream",
      inboundH: 4,
      outboundH: 9,
      resource: "web-existing-resource",
      unhandledOutboundEntries: [{ xml: "<message xmlns='jabber:client' id='dm-owned-sm-tail'/>", sentAt: "2026-08-08T12:34:56.789Z" }],
    });
    enqueueQueuedMessage("alice@example.com", {
      kind: "dm",
      id: "dm-owned-sm-tail",
      // Relative: the outbound queue drops entries older than QUEUE_TTL_MS
      // (7 days), so a pinned date silently expires the fixture.
      createdAt: new Date(Date.now() - 60_000).toISOString(),
      peerJid: "bob@example.com",
      body: "hello from superseded recovery",
    });

    const client = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      persistence,
    );
    const state = client as unknown as {
      reconnectBlock: { kind: "superseded"; detail: string } | null;
      loadModule: () => Promise<unknown>;
    };
    state.reconnectBlock = {
      kind: "superseded",
      detail: "This session was resumed in another tab.",
    };

    const boundResources: string[] = [];
    const resumeCalls: string[] = [];
    const drainedSends: Array<{ peerJid: string; id: string | null }> = [];
    const listeners = new Map<string, Array<(payload: { id?: string }) => void>>();
    let lifecycleCallback: ((event: string) => void) | undefined;
    class StubConfig {
      constructor(_url: string, _jid: string, _sessionId: string, resource: string) {
        boundResources.push(resource);
      }
      with_resume_state() { resumeCalls.push("with_resume_state"); }
      with_resume_state_with_max() { resumeCalls.push("with_resume_state_with_max"); }
      with_resume_state_entries() { resumeCalls.push("with_resume_state_entries"); }
      with_resume_state_entries_with_max() { resumeCalls.push("with_resume_state_entries_with_max"); }
      with_fresh_stream_retry_state_entries() { resumeCalls.push("with_fresh_stream_retry_state_entries"); }
    }
    class StubClient {
      set_on_session_lifecycle(callback: (event: string) => void) { lifecycleCallback = callback; }
      on(event: string, callback: (payload: { id?: string }) => void) {
        listeners.set(event, [...(listeners.get(event) ?? []), callback]);
      }
      async connect() {
        lifecycleCallback?.("fresh");
      }
      async send_chat_message(peerJid: string, _body: string, opts: { id?: string }) {
        const stanzaId = "dm-owned-sm-tail";
        drainedSends.push({ peerJid, id: stanzaId });
        for (const callback of listeners.get("message:acked") ?? []) {
          callback({ id: stanzaId });
        }
        return stanzaId;
      }
    }
    state.loadModule = async () => ({ WaddleConfig: StubConfig, WaddleClient: StubClient });

    try {
      await client.recoverSupersededSession();
      await flushMicrotasks();
      await (client as unknown as {
        flushQueuedDirectMessages: () => Promise<void | undefined>;
      }).flushQueuedDirectMessages();

      expect(boundResources).toHaveLength(1);
      expect(boundResources[0]).not.toBe("web-existing-resource");
      expect(client.fullJid).toBe(`alice@example.com/${boundResources[0]}`);
      expect(resumeCalls).toEqual([]);
      expect(drainedSends).toEqual([]);
      expect(listQueuedDmMessages("alice@example.com", "bob@example.com", "account").map((message) => message.id))
        .toEqual(["dm-owned-sm-tail"]);
      expect(persistence.smSnapshot()).toBeNull();
    } finally {
      removeQueuedMessage("alice@example.com", "dm-owned-sm-tail");
    }
  });

  test("BrowserXmppClient does not persist lossy SM state while outbound stanzas are unacked", () => {
    const persistence = inMemoryPersistence();
    persistence.saveJoinedRooms(["general@conference.example.com"]);
    const consumedClearCount = persistence.clearSmCount();
    const client = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      persistence,
    );
    (client as unknown as {
      xmpp: { get_resume_state: () => { previd: string; inboundH: number; outboundH: number; maxResumeSeconds: number; hasUnackedOutbound: boolean } };
    }).xmpp = {
      get_resume_state: () => ({
        previd: "live-sm-id",
        inboundH: 4,
        outboundH: 9,
        maxResumeSeconds: 300,
        hasUnackedOutbound: true,
      }),
    };

    client.persistResumeStateForPageHide();

    expect(persistence.smSnapshot()).toBeNull();
    expect(persistence.clearSmCount()).toBe(consumedClearCount + 1);
    expect(persistence.joinedRoomsSnapshot()).toEqual(["general@conference.example.com"]);
  });

  test("BrowserXmppClient persists SM state when unacked outbound stanzas are serializable", () => {
    const persistence = inMemoryPersistence();
    const client = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      persistence,
    );
    (client as unknown as {
      xmpp: {
        get_resume_state: () => {
          previd: string;
          inboundH: number;
          outboundH: number;
          maxResumeSeconds: number;
          hasUnackedOutbound: boolean;
          unhandledOutboundEntries: Array<{ xml: string; sentAt: string }>;
        };
      };
    }).xmpp = {
      get_resume_state: () => ({
        previd: "live-sm-id",
        inboundH: 4,
        outboundH: 9,
        maxResumeSeconds: 300,
        hasUnackedOutbound: true,
        unhandledOutboundEntries: [{ xml: "<message xmlns='jabber:client' id='unacked'/>", sentAt: "2026-07-26T12:34:56.789Z" }],
      }),
    };

    client.persistResumeStateForPageHide();

    expect(persistence.smSnapshot()).toMatchObject({
      previd: "live-sm-id",
      inboundH: 4,
      outboundH: 9,
      maxResumeSeconds: 300,
      unhandledOutboundEntries: [{ xml: "<message xmlns='jabber:client' id='unacked'/>", sentAt: "2026-07-26T12:34:56.789Z" }],
    });
  });

  test("BrowserXmppClient treats restored SM message stanzas as inflight queued sends", () => {
    const persistence = inMemoryPersistence();
    persistence.saveSm({
      previd: "live-sm-id",
      inboundH: 4,
      outboundH: 9,
      unhandledOutboundEntries: [{ xml: "<message xmlns='jabber:client' id='dm-live-1'/>", sentAt: "2026-07-26T12:34:56.789Z" }],
    });
    enqueueQueuedMessage("alice@example.com", {
      kind: "dm",
      id: "dm-live-1",
      createdAt: new Date().toISOString(),
      peerJid: "bob@example.com",
      body: "hello",
    });

    const client = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      persistence,
    );

    (client as unknown as { handleMessageAck: (id: string) => void }).handleMessageAck("dm-live-1");

    expect(listQueuedDmMessages("alice@example.com", "bob@example.com", "account")).toEqual([]);
  });

  test("a navigation clone cannot take the durable queue entry or its SM replay tail", () => {
    const ownerId = "queue-tail-owner";
    const messageId = "dm-owned-sm-tail";
    const state = {
      previd: "live-sm-id",
      inboundH: 4,
      outboundH: 9,
      resource: "web-original-resource",
      unhandledOutboundEntries: [{
        xml: `<message xmlns='jabber:client' id='${messageId}'/>`,
        sentAt: "2026-07-26T12:34:56.789Z",
      }],
    };
    seedCopiedOwnerHandoff(ownerId, state);
    enqueueQueuedMessage("alice@example.com", {
      kind: "dm",
      id: messageId,
      createdAt: new Date().toISOString(),
      peerJid: "bob@example.com",
      body: "still owned by the original tab",
    });
    const restoreNavigation = installNavigationTiming("navigate");

    try {
      const duplicate = createLocalStorageResumePersistence("alice@example.com");
      const client = new BrowserXmppClient(
        { jid: "alice@example.com", username: "alice" } as WaddleSession,
        duplicate,
      );

      expect(duplicate.loadSm()).toBeNull();
      expect(duplicate.consumeSm()).toBeNull();
      expect(client.fullJid).not.toBe("alice@example.com/web-original-resource");
      expect(listQueuedDmMessages("alice@example.com", "bob@example.com", "account").map((message) => message.id))
        .toContain(messageId);
      expect(createLocalStorageResumePersistence("alice@example.com", ownerId).consumeSm()).toEqual(state);
    } finally {
      restoreNavigation();
      removeQueuedMessage("alice@example.com", messageId);
    }
  });

  test("BrowserXmppClient retains restored SM queue entries through native replay failure until ack", () => {
    const persistence = inMemoryPersistence();
    persistence.saveSm({
      previd: "live-sm-id",
      inboundH: 4,
      outboundH: 9,
      unhandledOutboundEntries: [{ xml: "<message xmlns='jabber:client' id='dm-live-1'/>", sentAt: "2026-07-26T12:34:56.789Z" }],
    });
    enqueueQueuedMessage("alice@example.com", {
      kind: "dm",
      id: "dm-live-1",
      createdAt: new Date().toISOString(),
      peerJid: "bob@example.com",
      body: "hello",
    });

    const client = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      persistence,
    );

    (client as unknown as { handleMessageFailed: (id: string) => void }).handleMessageFailed("dm-live-1");

    expect(listQueuedDmMessages("alice@example.com", "bob@example.com", "account").map((message) => message.id)).toEqual([
      "dm-live-1",
    ]);

    (client as unknown as { handleMessageAck: (id: string) => void }).handleMessageAck("dm-live-1");
    expect(listQueuedDmMessages("alice@example.com", "bob@example.com", "account")).toEqual([]);
  });

  test("BrowserXmppClient persists SM state when the native replay queue is empty", () => {
    const persistence = inMemoryPersistence();
    const client = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      persistence,
    );
    (client as unknown as {
      xmpp: { get_resume_state: () => { previd: string; inboundH: number; outboundH: number; maxResumeSeconds: number; hasUnackedOutbound: boolean } };
    }).xmpp = {
      get_resume_state: () => ({
        previd: "live-sm-id",
        inboundH: 4,
        outboundH: 9,
        maxResumeSeconds: 300,
        hasUnackedOutbound: false,
      }),
    };

    client.persistResumeStateForPageHide();

    expect(persistence.smSnapshot()).toMatchObject({
      previd: "live-sm-id",
      inboundH: 4,
      outboundH: 9,
      maxResumeSeconds: 300,
    });
  });

  test("round-trips retained joined rooms for refresh-time group call discovery", () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com", "tab-a");
    const otherTab = createLocalStorageResumePersistence("alice@example.com", "tab-b");
    persistence.saveJoinedRooms([
      "General@Conference.Example.com/alice",
      "standup@conference.example.com",
      "standup@conference.example.com",
      "",
    ]);

    expect(persistence.loadJoinedRooms()).toEqual([
      "general@conference.example.com",
      "standup@conference.example.com",
    ]);
    expect(otherTab.loadJoinedRooms()).toEqual([]);
    persistence.clearJoinedRooms();
    expect(persistence.loadJoinedRooms()).toEqual([]);
  });

  test("round-trips terminal room auto-join blocks within the owning browser tab", () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com", "tab-a");
    const otherTab = createLocalStorageResumePersistence("alice@example.com", "tab-b");
    persistence.saveAutoJoinBlocks?.([
      {
        roomJid: "Private@Conference.Example.com/alice",
        condition: "registration-required",
        catalogFingerprint: "private@conference.example.com|private|space|1|0",
      },
      {
        roomJid: "private@conference.example.com",
        condition: "forbidden",
        catalogFingerprint: null,
      },
      {
        roomJid: "Limited@Conference.Example.com",
        condition: "forbidden",
        catalogFingerprint: "{\"spaceId\":\"space-a\"}",
        catalogFingerprintFields: ["spaceId", "isBookmarked"],
      },
    ]);

    expect(persistence.loadAutoJoinBlocks?.()).toEqual([
      {
        roomJid: "private@conference.example.com",
        condition: "forbidden",
        catalogFingerprint: null,
      },
      {
        roomJid: "limited@conference.example.com",
        condition: "forbidden",
        catalogFingerprint: "{\"spaceId\":\"space-a\"}",
        catalogFingerprintFields: ["spaceId", "isBookmarked"],
      },
    ]);
    expect(otherTab.loadAutoJoinBlocks?.()).toEqual([]);
    persistence.clearAutoJoinBlocks?.();
    expect(persistence.loadAutoJoinBlocks?.()).toEqual([]);
  });

  test("consumeSm rejects a delayed claimant that observed the same original resource", () => {
    const first = createLocalStorageResumePersistence("alice@example.com", "shared-owner");
    const second = createLocalStorageResumePersistence("alice@example.com", "shared-owner");
    const state: PersistedSmResumeState = {
      previd: "abc-123",
      inboundH: 42,
      outboundH: 7,
      maxResumeSeconds: 300,
      resource: "web-existing-resource",
    };
    first.saveSm(state);

    const storage = window.localStorage;
    const originalSetItem = storage.setItem.bind(storage);
    let reentered = false;
    let secondResult: PersistedSmResumeState | null | undefined;
    storage.setItem = ((key: string, value: string) => {
      if (
        key === smOwnerKey("alice@example.com", "shared-owner")
        && value.includes('"claimId"')
        && !reentered
      ) {
        reentered = true;
        secondResult = second.consumeSm();
      }
      originalSetItem(key, value);
    }) as Storage["setItem"];

    try {
      const firstResult = first.consumeSm();

      expect(secondResult).toEqual(state);
      expect(firstResult).toBeNull();
      expect(first.loadSm()).toBeNull();
      expect(first.consumeSm()).toBeNull();
    } finally {
      storage.setItem = originalSetItem;
    }
  });

  test("SM TTL: an old envelope without advertised max uses the default resume window", () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com", "ttl-default-owner");
    const justOverDefaultResumeWindow = Date.now() - 301_000;
    window.localStorage.setItem(
      smOwnerKey("alice@example.com", "ttl-default-owner"),
      JSON.stringify({ previd: "stale", inboundH: 1, outboundH: 1, savedAt: justOverDefaultResumeWindow, ownerId: "ttl-default-owner" }),
    );
    expect(persistence.loadSm()).toBeNull();
    // Stale entries are pruned on read so the next load doesn't
    // pay the validation cost again.
    expect(window.localStorage.getItem(smOwnerKey("alice@example.com", "ttl-default-owner"))).toBeNull();
  });

  test("SM TTL: advertised maxResumeSeconds controls persisted resume expiry", () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com", "ttl-advertised-owner");
    const twoSecondsAgo = Date.now() - 2_000;
    window.localStorage.setItem(
      smOwnerKey("alice@example.com", "ttl-advertised-owner"),
      JSON.stringify({
        previd: "stale",
        inboundH: 1,
        outboundH: 1,
        maxResumeSeconds: 1,
        savedAt: twoSecondsAgo,
        ownerId: "ttl-advertised-owner",
      }),
    );

    expect(persistence.consumeSm()).toBeNull();
    expect(window.localStorage.getItem(smOwnerKey("alice@example.com", "ttl-advertised-owner"))).toBeNull();
  });

  test("SM TTL: future-dated envelopes fail closed and are pruned", () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com", "ttl-future-owner");
    const farFuture = Date.now() + 120_000;
    window.localStorage.setItem(
      smOwnerKey("alice@example.com", "ttl-future-owner"),
      JSON.stringify({
        previd: "from-the-future",
        inboundH: 1,
        outboundH: 1,
        maxResumeSeconds: 300,
        savedAt: farFuture,
        ownerId: "ttl-future-owner",
      }),
    );

    expect(persistence.loadSm()).toBeNull();
    expect(window.localStorage.getItem(smOwnerKey("alice@example.com", "ttl-future-owner"))).toBeNull();
  });

  test("SM state rejects non-u32 stanza counters", () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com", "bad-counter-owner");
    window.localStorage.setItem(
      smOwnerKey("alice@example.com", "bad-counter-owner"),
      JSON.stringify({
        previd: "bad-counter",
        inboundH: 1.5,
        outboundH: 1,
        savedAt: Date.now(),
        ownerId: "bad-counter-owner",
      }),
    );

    expect(persistence.loadSm()).toBeNull();
  });

  test("loadCatchup returns null for malformed JSON", () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com", "tab-a");
    window.localStorage.setItem(catchupShardKey("alice@example.com", "tab-a"), "{not valid json");
    expect(persistence.loadCatchup()).toBeNull();
  });

  test("loadCatchup rejects payloads with the wrong shape", () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com", "tab-a");
    window.localStorage.setItem(
      catchupShardKey("alice@example.com", "tab-a"),
      JSON.stringify({ dmLastSeen: "not-an-array", roomLastSeen: [] }),
    );
    expect(persistence.loadCatchup()).toBeNull();
  });

  test("two tab shards preserve concurrent cursor writes", async () => {
    // Simulate two `ReconnectCatchup` instances sharing the same
    // localStorage (two tabs of the same account). Each advances
    // its own conversation; after both persist, BOTH conversations
    // are present in storage — the pre-fix behavior would have
    // overwritten one with the other.
    const persistenceA = createLocalStorageResumePersistence("alice@example.com", "tab-a");
    const persistenceB = createLocalStorageResumePersistence("alice@example.com", "tab-b");
    const tabA = new ReconnectCatchup(persistenceA);
    const tabB = new ReconnectCatchup(persistenceB);
    tabA.recordDmSeen("room@rooms.waddle.example/alice", "2026-05-20T10:00:00Z", "dm-arch-A", [], "muc-occupant");
    tabB.recordRoomSeen("general@muc.example.com", "2026-05-20T11:00:00Z", "room-arch-B");
    await new Promise((resolve) => setTimeout(resolve, 0));
    const final = persistenceA.loadCatchup();
    expect(final).not.toBeNull();
    expect(final!.dmLastSeen).toContainEqual([
      "room@rooms.waddle.example/alice",
      expect.objectContaining({ scope: "muc-occupant" }),
    ]);
    expect(final!.roomLastSeen.map((e) => e[0])).toContain("general@muc.example.com");
    expect(window.localStorage.getItem(catchupShardKey("alice@example.com", "tab-a"))).not.toBeNull();
    expect(window.localStorage.getItem(catchupShardKey("alice@example.com", "tab-b"))).not.toBeNull();
  });

  test("account shard prefixes cannot read or clear a longer JID", () => {
    const alice = createLocalStorageResumePersistence("alice@example.com", "tab-a");
    const evil = createLocalStorageResumePersistence("alice@example.com.evil", "tab-b");
    alice.saveCatchup({
      dmLastSeen: [["bob@example.com", { timestamp: "2026-05-20T10:00:00.000Z" }]],
      roomLastSeen: [],
    });
    evil.saveCatchup({
      dmLastSeen: [["mallory@example.com.evil", { timestamp: "2026-05-20T11:00:00.000Z" }]],
      roomLastSeen: [],
    });

    expect(alice.loadCatchup()?.dmLastSeen.map(([key]) => key)).toEqual(["bob@example.com"]);
    alice.clearCatchup();
    expect(alice.loadCatchup()).toBeNull();
    expect(evil.loadCatchup()?.dmLastSeen.map(([key]) => key)).toEqual(["mallory@example.com.evil"]);
  });
});
