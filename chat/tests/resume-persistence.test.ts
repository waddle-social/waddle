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
import {
  BrowserXmppClient,
  isStaleOwnerDisposeError,
  type BrowserXmppClientOptions,
} from "../src/lib/xmpp/client";
import {
  ResumeStateTeardownError,
  applyResumeStateToWasmConfig,
} from "../src/lib/xmpp/client-connection";
import { installXmppPagehideLifecycle } from "../src/lib/xmpp/pagehide-lifecycle";
import { enqueueQueuedMessage, listQueuedDmMessages } from "../src/lib/outbound-queue-store";
import type { WaddleSession } from "../src/lib/server-auth";
import {
  OutboundPersistenceError,
  type DurableOutcome,
} from "../src/lib/xmpp-runtime/durable-contract";
import {
  MemoryDurableOutboundStore,
  MemoryDurableOutboundStore as MemoryDurableSmResumeStore,
} from "../src/lib/xmpp-runtime/memory-durable-store";
import {
  createLocalStorageResumePersistence as createBrowserLocalStorageResumePersistence,
  nullResumePersistence,
  XmppLifecycleId,
  type PersistedReconnectCatchup,
  type PersistedSmResumeState,
  type ResumePersistence,
  type XmppResumeEntry,
  type XmppResumeStanzaKind,
} from "../src/lib/xmpp/resume-persistence";

function createLocalStorageResumePersistence(
  accountKey: string,
  ownerId?: string,
  durableRuntimeStore = new MemoryDurableOutboundStore(),
  lifecycleId = XmppLifecycleId.create(),
): ResumePersistence {
  return createBrowserLocalStorageResumePersistence(
    accountKey,
    ownerId,
    durableRuntimeStore,
    lifecycleId,
  );
}

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
const RESUME_SENT_AT_EPOCH_MS = Date.parse("2026-07-16T08:09:10.123Z");

function resumeEntry(kind: XmppResumeStanzaKind, id?: string): XmppResumeEntry {
  return {
    stanza: {
      stanzaKind: kind,
      tokens: [
        {
          kind: "start",
          name: { namespace: "jabber:client", localName: kind },
          attributes: id
            ? [{ name: { namespace: "", localName: "id" }, value: id }]
            : [],
        },
        { kind: "end" },
      ],
    },
    sentAtEpochMs: RESUME_SENT_AT_EPOCH_MS,
  };
}
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

let createdClients: BrowserXmppClient[] = [];

function expectStaleOwnerDisposeError(reason: unknown): void {
  if (!isStaleOwnerDisposeError(reason)) {
    throw new Error("expected an exact stale-owner disposal error");
  }
  expect(reason.transportDisposed).toBe(true);
  expect(reason.failures.every(({ stage }) => (
    stage === "sm-clear" || stage === "native-claim-release"
  ))).toBe(true);
  const smFailure = reason.failures.find(({ stage }) => stage === "sm-clear");
  expect(smFailure?.cause).toBeInstanceOf(ResumeStateTeardownError);
  const resumeFailure = smFailure!.cause as ResumeStateTeardownError;
  expect(resumeFailure.failures).toHaveLength(1);
  expect(resumeFailure.failures[0]?.stage).toBe("sm-clear");
  const error = resumeFailure.failures[0]?.cause;
  expect(error).toBeInstanceOf(OutboundPersistenceError);
  const persistenceError = error as OutboundPersistenceError;
  expect(persistenceError.operation).toBe("sm-clear-all");
  expect(persistenceError.reason).toBe("aborted");
  expect(persistenceError.cause).toBeInstanceOf(DOMException);
  const cause = persistenceError.cause as DOMException;
  expect(cause.name).toBe("AbortError");
  expect([
    "SM owner fenced during clear",
    "SM snapshot changed during clear",
  ]).toContain(cause.message);
  for (const failure of reason.failures) {
    if (failure.stage !== "native-claim-release") continue;
    expect(failure.cause).toBeInstanceOf(DOMException);
    expect((failure.cause as DOMException).name).toBe("AbortError");
    expect((failure.cause as DOMException).message).toBe(
      "Outbound owner fenced during native reconciliation",
    );
  }
}

afterEach(async () => {
  try {
    // These tests deliberately transfer SM ownership between simulated tabs.
    // A predecessor's final disconnect may therefore be fenced while clearing
    // its stale snapshot. Every other disposal failure remains a test failure.
    const disposals = await Promise.allSettled(
      createdClients.map((client) => {
        (client as unknown as { xmpp: null }).xmpp = null;
        return client.dispose();
      }),
    );
    for (const disposal of disposals) {
      if (disposal.status === "rejected") {
        expectStaleOwnerDisposeError(disposal.reason);
      }
    }
  } finally {
    createdClients = [];
    const g = globalThis as ShimmedGlobal;
    if (g.window?.[WINDOW_SENTINEL]) {
      g.window.localStorage.clear();
      g.window.sessionStorage.clear();
    }
  }
});

/** In-memory persistence for tests — same shape as the real adapter
 * but without touching localStorage. */
function inMemoryPersistence(): ResumePersistence & {
  catchupSnapshot: () => PersistedReconnectCatchup | null;
  smSnapshot: () => PersistedSmResumeState | null;
  joinedRoomsSnapshot: () => string[];
  clearSmCount: () => number;
} {
  let catchup: PersistedReconnectCatchup | null = null;
  let sm: PersistedSmResumeState | null = null;
  let joinedRooms: string[] = [];
  let smClears = 0;
  const lifecycleId = XmppLifecycleId.create();
  const durableRuntimeStore = new MemoryDurableOutboundStore();
  return {
    lifecycleId,
    durableRuntimeStore,
    loadCatchup: () => catchup,
    saveCatchup: (snapshot) => { catchup = snapshot; },
    clearCatchup: () => { catchup = null; },
    loadSm: async () => ({ kind: "committed", value: sm }),
    consumeSm: async () => {
      const current = sm;
      sm = null;
      return { kind: "committed", value: current };
    },
    saveSm: async (state) => {
      sm = state;
      return { kind: "committed", value: undefined };
    },
    clearSm: async () => {
      smClears += 1;
      const removed = sm !== null;
      sm = null;
      return { kind: "committed", value: removed };
    },
    outboundOwnerHint: () => ({
      ownerId: "memory-owner",
      ownerInstanceId: lifecycleId.value,
    }),
    acceptOutboundOwner: () => undefined,
    preparePagehideHandoff: async (state) => {
      if (state === null) smClears += 1;
      sm = state;
      return {
        kind: "committed",
        value: {
          handoff: {
            token: "memory-handoff",
            expiresAt: Date.now() + 30_000,
            authorityEpoch: 1,
            ownerGeneration: 1,
          },
          smVersion: 1,
        },
      };
    },
    publishPagehideHandoff: () => undefined,
    reclaimPagehideOwnership: async () => ({
      kind: "committed",
      value: undefined,
    }),
    loadJoinedRooms: () => [...joinedRooms],
    saveJoinedRooms: (rooms) => { joinedRooms = [...rooms]; },
    clearJoinedRooms: () => { joinedRooms = []; },
    catchupSnapshot: () => catchup,
    smSnapshot: () => sm,
    joinedRoomsSnapshot: () => [...joinedRooms],
    clearSmCount: () => smClears,
  };
}

function mockPersistenceOptions(
  persistence: ResumePersistence,
  durableRuntimeStore = persistence.durableRuntimeStore
    ?? new MemoryDurableOutboundStore(),
): BrowserXmppClientOptions {
  return {
    durableRuntimeStore,
    createResumePersistence: (lifecycleId, sharedStore) => ({
      ...persistence,
      lifecycleId,
      durableRuntimeStore: sharedStore,
      outboundOwnerHint: () => ({
        ...persistence.outboundOwnerHint(),
        ownerInstanceId: lifecycleId.value,
      }),
    }),
  };
}

function localPersistenceOptions(
  accountKey: string,
  ownerId: string | undefined,
  durableRuntimeStore: MemoryDurableOutboundStore,
): BrowserXmppClientOptions {
  return {
    durableRuntimeStore,
    createResumePersistence: (lifecycleId, sharedStore) => (
      createLocalStorageResumePersistence(
        accountKey,
        ownerId,
        sharedStore,
        lifecycleId,
      )
    ),
  };
}

async function committed<T>(operation: Promise<DurableOutcome<T>>): Promise<T> {
  const outcome = await operation;
  expect(outcome.kind).toBe("committed");
  if (outcome.kind !== "committed") throw new Error(`durable operation failed: ${outcome.reason}`);
  return outcome.value;
}

async function waitForClientStartup(client: BrowserXmppClient): Promise<void> {
  if (!createdClients.includes(client)) createdClients.push(client);
  await (client as unknown as { outboundQueueHydration: Promise<void> }).outboundQueueHydration;
}

async function seedRawSm(
  store: MemoryDurableOutboundStore,
  ownerId: string,
  state: PersistedSmResumeState,
  savedAt: number,
): Promise<void> {
  const activation = await committed(store.claimOwner("alice@example.com", {
    ownerId,
    ownerInstanceId: `explicit:${ownerId}`,
  }));
  const owner = activation.fence;
  const loaded = await committed(store.loadSm(owner));
  if (loaded.kind !== "loaded") throw new Error("owner fenced while seeding SM");
  const saved = await committed(
    store.saveSm(owner, loaded.version, state, savedAt),
  );
  if (saved.kind !== "applied") throw new Error("SM seed CAS failed");
}

async function expectRawSmRejected(
  store: MemoryDurableOutboundStore,
  ownerId: string,
  state: PersistedSmResumeState,
  savedAt: number,
): Promise<void> {
  const activation = await committed(store.claimOwner("alice@example.com", {
    ownerId,
    ownerInstanceId: `explicit:${ownerId}`,
  }));
  const owner = activation.fence;
  const loaded = await committed(store.loadSm(owner));
  if (loaded.kind !== "loaded") throw new Error("owner fenced while rejecting SM seed");
  const saved = await store.saveSm(owner, loaded.version, state, savedAt);
  expect(saved.kind).toBe("failed");
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
  test("hydrates DM + room cursors from the persistence snapshot", async () => {
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

  test("canonicalized hydrate collisions merge cursor progress instead of using persistence order", async () => {
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

  test("hydrated instance returns cursors on the *first* onSessionStarted (PR3 whole point)", async () => {
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

  test("a fresh (un-hydrated) instance still treats the first onSessionStarted as initial login", async () => {
    // Unchanged from pre-PR3 behavior: a brand-new account / first-
    // ever login has nothing to catch up on.
    const c = new ReconnectCatchup(nullResumePersistence);
    expect(c.onSessionStarted()).toEqual([]);
  });

  test("missing snapshot is a no-op (legacy / first-ever load)", async () => {
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
  function recordingConfig() {
    const calls: unknown[] = [];
    return {
      config: { with_resume_state: (state: unknown) => calls.push(state) },
      calls,
    };
  }

  test("passes one exact typed timestamped snapshot and keeps resource browser-only", () => {
    const { config, calls } = recordingConfig();

    applyResumeStateToWasmConfig(config, {
      previd: "prev-1",
      inboundH: 7,
      outboundH: 11,
      maxResumeSeconds: 300,
      unhandledOutboundEntries: [resumeEntry("message")],
      resource: "web-browser-only",
    });

    expect(calls).toEqual([
      {
        previd: "prev-1",
        inboundH: 7,
        outboundH: 11,
        maxResumeSeconds: 300,
        unhandledOutboundEntries: [resumeEntry("message")],
      },
    ]);
  });

  test("passes an explicit empty ordered entry array", () => {
    const { config, calls } = recordingConfig();

    applyResumeStateToWasmConfig(config, {
      previd: "prev-2",
      inboundH: 3,
      outboundH: 5,
      maxResumeSeconds: 120,
      unhandledOutboundEntries: [],
    });

    expect(calls).toEqual([
      {
        previd: "prev-2",
        inboundH: 3,
        outboundH: 5,
        maxResumeSeconds: 120,
        unhandledOutboundEntries: [],
      },
    ]);
  });

  test("rejects a snapshot whose ordered entry array is absent", () => {
    const { config } = recordingConfig();
    expect(() => applyResumeStateToWasmConfig(config, {
      previd: "prev-missing-entries",
      inboundH: 1,
      outboundH: 2,
    } as never)).toThrow("unhandledOutboundEntries must be an ordered array");
  });
});

describe("createLocalStorageResumePersistence — localStorage adapter", () => {
  test("round-trips a catchup snapshot through localStorage", async () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com");
    const snapshot: PersistedReconnectCatchup = {
      dmLastSeen: [["bob@example.com", { timestamp: "2026-05-20T10:00:00.000Z" }]],
      roomLastSeen: [],
    };
    persistence.saveCatchup(snapshot);
    expect(persistence.loadCatchup()).toEqual(snapshot);
  });

  test("round-trips an SM resume state through localStorage (with internal savedAt)", async () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com");
    const state = {
      previd: "abc-123",
      inboundH: 42,
      outboundH: 7,
      unhandledOutboundEntries: [resumeEntry("message", "m1")],
    };
    await committed(persistence.saveSm(state));
    // Round-trip strips the internal `savedAt` so the caller gets
    // the same shape it passed in.
    expect(await committed(persistence.loadSm())).toEqual(state);
  });

  test("round-trips the bound resource with SM resume state", async () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com");
    const state = {
      previd: "abc-123",
      inboundH: 42,
      outboundH: 7,
      unhandledOutboundEntries: [],
      resource: "web-existing-resource",
    };
    await committed(persistence.saveSm(state));
    expect(await committed(persistence.loadSm())).toEqual(state);
  });

  test("consumeSm claims and clears the stored resource for only one client", async () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com");
    const state = {
      previd: "abc-123",
      inboundH: 42,
      outboundH: 7,
      unhandledOutboundEntries: [],
      resource: "web-existing-resource",
    };
    await committed(persistence.saveSm(state));
    expect(await committed(persistence.consumeSm())).toEqual(state);
    expect(await committed(persistence.consumeSm())).toBeNull();
    expect(await committed(persistence.loadSm())).toBeNull();
  });

  test("consumeSm is scoped to the tab owner", async () => {
    const store = new MemoryDurableOutboundStore();
    const tabA = createLocalStorageResumePersistence("alice@example.com", "tab-a", store);
    const tabB = createLocalStorageResumePersistence("alice@example.com", "tab-b", store);
    const state = {
      previd: "abc-123",
      inboundH: 42,
      outboundH: 7,
      unhandledOutboundEntries: [],
      resource: "web-existing-resource",
    };
    await committed(tabA.saveSm(state));

    expect(await committed(tabB.loadSm())).toBeNull();
    expect(await committed(tabB.consumeSm())).toBeNull();
    expect(await committed(tabA.consumeSm())).toEqual(state);
    expect(await committed(tabA.consumeSm())).toBeNull();
  });

  test("a duplicated tab rotates a copied live owner before it can claim SM state", async () => {
    const store = new MemoryDurableOutboundStore();
    const copiedOwner = "copied-live-owner";
    const state = {
      previd: "abc-123",
      inboundH: 42,
      outboundH: 7,
      unhandledOutboundEntries: [],
      resource: "web-existing-resource",
    };
    const originalTab = createLocalStorageResumePersistence(
      "alice@example.com",
      copiedOwner,
      store,
    );
    await committed(originalTab.saveSm(state));
    window.sessionStorage.setItem("waddle.chat.sm-resume.owner", copiedOwner);
    window.localStorage.setItem(
      `waddle.chat.sm-resume.owner-lease.${copiedOwner}`,
      JSON.stringify({
        ownerId: copiedOwner,
        instanceId: "original-live-tab",
        updatedAt: Date.now(),
      }),
    );

    const duplicatedTab = createLocalStorageResumePersistence("alice@example.com", undefined, store);

    expect(await committed(duplicatedTab.consumeSm())).toBeNull();
    expect(await committed(originalTab.consumeSm())).toEqual(state);
  });

  test("a pagehide handoff keeps the copied owner for same-tab reload consumption", async () => {
    const store = new MemoryDurableOutboundStore();
    const reloadOwner = "reload-owner";
    const state = {
      previd: "abc-123",
      inboundH: 42,
      outboundH: 7,
      unhandledOutboundEntries: [],
      resource: "web-existing-resource",
    };
    const previousPage = createLocalStorageResumePersistence("alice@example.com", reloadOwner, store);
    await committed(previousPage.saveSm(state));
    const receipt = await committed(previousPage.preparePagehideHandoff(state));
    previousPage.publishPagehideHandoff(receipt);
    const handoff = previousPage.outboundOwnerHint();
    expect(handoff.handoffToken).toBeString();
    window.sessionStorage.setItem("waddle.chat.sm-resume.owner", reloadOwner);
    window.sessionStorage.setItem(
      "waddle.chat.sm-resume.owner-handoff-token",
      handoff.handoffToken!,
    );

    const reloadedPage = createLocalStorageResumePersistence("alice@example.com", undefined, store);

    expect(await committed(reloadedPage.consumeSm())).toEqual(state);
  });

  test("BFCache pagehide persists before eviction and pageshow reclaims owner handoff", async () => {
    const ownerId = "bfcache-owner";
    const durableRuntimeStore = new MemoryDurableOutboundStore();
    window.sessionStorage.setItem("waddle.chat.sm-resume.owner", ownerId);
    let persistence: ResumePersistence | undefined;
    const client = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      {
        durableRuntimeStore,
        createResumePersistence: (lifecycleId, sharedStore) => {
          persistence = createLocalStorageResumePersistence(
            "alice@example.com",
            undefined,
            sharedStore,
            lifecycleId,
          );
          return persistence;
        },
      },
    );
    await waitForClientStartup(client);
    if (!persistence) throw new Error("BFCache persistence was not constructed");
    const activeResource = (client as unknown as { resource: string }).resource;
    (client as unknown as {
      xmpp: {
        request_stream_management_ack: () => Promise<void>;
        get_resume_state: () => PersistedSmResumeState;
      };
    }).xmpp = {
      request_stream_management_ack: async () => undefined,
      get_resume_state: () => ({
        previd: "bfcache-stream",
        inboundH: 12,
        outboundH: 8,
        maxResumeSeconds: 300,
        resource: "web-bfcache",
        unhandledOutboundEntries: [],
      }),
    };

    const listeners = new Map<string, EventListener>();
    const target = {
      addEventListener: (type: string, listener: EventListener) => listeners.set(type, listener),
      removeEventListener: (type: string) => listeners.delete(type),
    };
    const remove = installXmppPagehideLifecycle(
      target as unknown as Window,
      () => client,
      () => {
        throw new Error("BFCache pagehide must not suspend call media");
      },
    );
    const dispatch = (type: "pagehide" | "pageshow") => {
      listeners.get(type)?.({ persisted: true } as PageTransitionEvent);
    };
    const waitForPageLifecycle = () => (
      client as unknown as {
        resume: { whenPageLifecycleQuiescent(): Promise<void> };
      }
    ).resume.whenPageLifecycleQuiescent();

    dispatch("pagehide");
    await waitForPageLifecycle();
    expect(await committed(persistence.loadSm())).toMatchObject({
      previd: "bfcache-stream",
      inboundH: 12,
      outboundH: 8,
    });
    dispatch("pageshow");
    await waitForPageLifecycle();

    // A later BFCache eviction creates a fresh JS context. The synchronous
    // pagehide snapshot remains consumable and preserves the bound resource.
    dispatch("pagehide");
    await waitForPageLifecycle();
    const secondHandoff = persistence.outboundOwnerHint();
    expect(secondHandoff.handoffToken).toBeString();
    expect(window.sessionStorage.getItem("waddle.chat.sm-resume.owner")).toBe(ownerId);
    expect(window.sessionStorage.getItem("waddle.chat.sm-resume.owner-handoff-token"))
      .toBe(secondHandoff.handoffToken!);
    expect(await committed(persistence.loadSm())).toMatchObject({
      resource: activeResource,
    });
    const reloaded = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      localPersistenceOptions(
        "alice@example.com",
        undefined,
        durableRuntimeStore,
      ),
    );
    await waitForClientStartup(reloaded);
    expect((reloaded as unknown as { resource: string }).resource).toBe(activeResource);
    remove();
  });

  test("final dispose by a fenced predecessor cannot mutate successor SM or outbound rows", async () => {
    const account = "alice@example.com";
    const ownerId = "dispose-handoff-owner";
    const store = new MemoryDurableOutboundStore();
    let predecessorPersistence: ResumePersistence | undefined;
    const predecessor = new BrowserXmppClient(
      { jid: account, username: "alice" } as WaddleSession,
      {
        durableRuntimeStore: store,
        createResumePersistence: (lifecycleId, durableRuntimeStore) => {
          expect(durableRuntimeStore).toBe(store);
          predecessorPersistence = createLocalStorageResumePersistence(
            account,
            ownerId,
            durableRuntimeStore,
            lifecycleId,
          );
          return predecessorPersistence;
        },
      },
    );
    await waitForClientStartup(predecessor);
    if (!predecessorPersistence) {
      throw new Error("predecessor persistence was not constructed");
    }

    const row = {
      kind: "dm" as const,
      id: "successor-row",
      createdAt: "2026-07-17T12:00:00.000Z",
      peerJid: "bob@example.com",
      body: "durable across predecessor disposal",
    };
    expect(await committed(store.persistReady(account, row))).toMatchObject({
      kind: "inserted",
    });

    const predecessorState: PersistedSmResumeState = {
      previd: "predecessor-stream",
      inboundH: 3,
      outboundH: 5,
      unhandledOutboundEntries: [],
      resource: "web-predecessor",
    };
    await committed(predecessorPersistence.saveSm(predecessorState));
    const receipt = await committed(
      predecessorPersistence.preparePagehideHandoff(predecessorState),
    );
    predecessorPersistence.publishPagehideHandoff(receipt);
    const handoff = predecessorPersistence.outboundOwnerHint();
    expect(handoff.handoffToken).toBeString();
    window.sessionStorage.setItem("waddle.chat.sm-resume.owner", ownerId);
    window.sessionStorage.setItem(
      "waddle.chat.sm-resume.owner-handoff-token",
      handoff.handoffToken!,
    );

    let successorPersistence: ResumePersistence | undefined;
    const successor = new BrowserXmppClient(
      { jid: account, username: "alice" } as WaddleSession,
      {
        durableRuntimeStore: store,
        createResumePersistence: (lifecycleId, durableRuntimeStore) => {
          expect(durableRuntimeStore).toBe(store);
          successorPersistence = createLocalStorageResumePersistence(
            account,
            undefined,
            durableRuntimeStore,
            lifecycleId,
          );
          return successorPersistence;
        },
      },
    );
    await waitForClientStartup(successor);
    if (!successorPersistence) {
      throw new Error("successor persistence was not constructed");
    }
    expect((successor as unknown as { resource: string }).resource).toBe(
      predecessorState.resource!,
    );
    const successorState: PersistedSmResumeState = {
      previd: "successor-stream",
      inboundH: 8,
      outboundH: 13,
      unhandledOutboundEntries: [],
      resource: "web-successor",
    };
    await committed(successorPersistence.saveSm(successorState));
    const rowsBeforeDispose = await committed(store.list(account));

    let disposeError: unknown;
    try {
      await predecessor.dispose();
    } catch (error) {
      disposeError = error;
    }
    expectStaleOwnerDisposeError(disposeError);
    expect(isStaleOwnerDisposeError(new Error("unrelated disposal failure"))).toBe(false);

    expect(await committed(successorPersistence.loadSm())).toEqual(
      successorState,
    );
    expect(await committed(store.list(account))).toEqual(rowsBeforeDispose);
    expect(rowsBeforeDispose).toEqual([row]);
  });

  test("predecessor local cleanup cannot erase successor catch-up or joined-room shards", async () => {
    const account = "alice@example.com";
    const ownerId = "generation-sharded-local-state";
    const store = new MemoryDurableOutboundStore();
    const predecessor = createLocalStorageResumePersistence(
      account,
      ownerId,
      store,
      XmppLifecycleId.explicitForTest("local-predecessor"),
    );
    await committed(predecessor.saveSm({
      previd: "predecessor-stream",
      inboundH: 1,
      outboundH: 2,
      unhandledOutboundEntries: [],
    }));
    predecessor.saveCatchup({
      dmLastSeen: [["old@example.com", {
        timestamp: "2026-07-17T10:00:00.000Z",
      }]],
      roomLastSeen: [],
    });
    predecessor.saveJoinedRooms(["old@muc.example.com"]);
    const receipt = await committed(
      predecessor.preparePagehideHandoff({
        previd: "predecessor-stream",
        inboundH: 1,
        outboundH: 2,
        unhandledOutboundEntries: [],
      }),
    );
    predecessor.publishPagehideHandoff(receipt);

    const successorLifecycle = XmppLifecycleId.explicitForTest(
      "local-successor",
    );
    const successor = createLocalStorageResumePersistence(
      account,
      ownerId,
      store,
      successorLifecycle,
    );
    const activation = await committed(store.claimOwner(account, {
      ownerId,
      ownerInstanceId: successorLifecycle.value,
      handoffToken: receipt.handoff.token,
    }));
    successor.acceptOutboundOwner(activation);
    successor.saveCatchup({
      dmLastSeen: [["new@example.com", {
        timestamp: "2026-07-17T11:00:00.000Z",
      }]],
      roomLastSeen: [],
    });
    successor.saveJoinedRooms(["new@muc.example.com"]);

    predecessor.clearCatchup();
    predecessor.clearJoinedRooms();

    expect(successor.loadCatchup()).toEqual({
      dmLastSeen: [["new@example.com", {
        timestamp: "2026-07-17T11:00:00.000Z",
      }]],
      roomLastSeen: [],
    });
    expect(successor.loadJoinedRooms()).toEqual([
      "new@muc.example.com",
    ]);
  });

  test("a slow same-tab reload keeps the owner until the prior lease expires", async () => {
    const realNow = Date.now;
    let now = 1_000_000;
    Date.now = () => now;

    try {
      const store = new MemoryDurableOutboundStore();
      const reloadOwner = "slow-reload-owner";
      const state = {
        previd: "abc-123",
        inboundH: 42,
        outboundH: 7,
        unhandledOutboundEntries: [],
        resource: "web-existing-resource",
      };
      const previousPage = createLocalStorageResumePersistence("alice@example.com", reloadOwner, store);
      await committed(previousPage.saveSm(state));
      previousPage.saveJoinedRooms(["general@conference.example.com"]);
      const receipt = await committed(previousPage.preparePagehideHandoff(state));
      previousPage.publishPagehideHandoff(receipt);
      const handoff = previousPage.outboundOwnerHint();
      expect(handoff.handoffToken).toBeString();
      window.sessionStorage.setItem("waddle.chat.sm-resume.owner", reloadOwner);
      window.sessionStorage.setItem(
        "waddle.chat.sm-resume.owner-handoff-token",
        handoff.handoffToken!,
      );

      now += 15_000;
      const reloadedPage = createLocalStorageResumePersistence("alice@example.com", undefined, store);

      expect(reloadedPage.loadJoinedRooms()).toEqual(["general@conference.example.com"]);
      expect(await committed(reloadedPage.consumeSm())).toEqual(state);
    } finally {
      Date.now = realNow;
    }
  });

  test("BrowserXmppClient only reuses a refreshed resource for the owning tab", async () => {
    const state = {
      previd: "abc-123",
      inboundH: 42,
      outboundH: 7,
      unhandledOutboundEntries: [],
      resource: "web-existing-resource",
    };
    const durableRuntimeStore = new MemoryDurableOutboundStore();
    window.sessionStorage.setItem("waddle.chat.sm-resume.owner", "tab-a");
    const previousTabA = createLocalStorageResumePersistence(
      "alice@example.com",
      undefined,
      durableRuntimeStore,
    );
    await committed(previousTabA.saveSm(state));
    const receipt = await committed(previousTabA.preparePagehideHandoff(state));
    previousTabA.publishPagehideHandoff(receipt);

    const tabB = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      localPersistenceOptions(
        "alice@example.com",
        "tab-b",
        durableRuntimeStore,
      ),
    );
    await waitForClientStartup(tabB);
    expect(tabB.fullJid).not.toBe("alice@example.com/web-existing-resource");

    const tabA = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      localPersistenceOptions(
        "alice@example.com",
        undefined,
        durableRuntimeStore,
      ),
    );
    await waitForClientStartup(tabA);
    expect(tabA.fullJid).toBe("alice@example.com/web-existing-resource");
  });

  test("BrowserXmppClient rejects split resume and outbound durable stores", () => {
    const persistenceStore = new MemoryDurableOutboundStore();
    const outboundStore = new MemoryDurableOutboundStore();
    expect(() => new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      {
        durableRuntimeStore: outboundStore,
        createResumePersistence: (lifecycleId) => (
          createLocalStorageResumePersistence(
            "alice@example.com",
            "tab-a",
            persistenceStore,
            lifecycleId,
          )
        ),
      },
    )).toThrow("must share one durable runtime store");
  });

  test("BrowserXmppClient rejects live SM state without the ordered entry array", async () => {
    const persistence = inMemoryPersistence();
    persistence.saveJoinedRooms(["general@conference.example.com"]);
    const consumedClearCount = persistence.clearSmCount();
    const client = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      mockPersistenceOptions(persistence),
    );
    await waitForClientStartup(client);
    (client as unknown as {
      xmpp: { get_resume_state: () => unknown };
    }).xmpp = {
      get_resume_state: () => ({
        previd: "live-sm-id",
        inboundH: 4,
        outboundH: 9,
        maxResumeSeconds: 300,
      }),
    };

    expect(() => client.persistResumeStateForPageHide())
      .toThrow("unhandledOutboundEntries must be an ordered array");

    expect(persistence.smSnapshot()).toBeNull();
    expect(persistence.clearSmCount()).toBe(consumedClearCount);
    expect(persistence.joinedRoomsSnapshot()).toEqual(["general@conference.example.com"]);
  });

  test("BrowserXmppClient persists SM state when unacked outbound stanzas are serializable", async () => {
    const persistence = inMemoryPersistence();
    const client = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      mockPersistenceOptions(persistence),
    );
    await waitForClientStartup(client);
    (client as unknown as {
      xmpp: {
        get_resume_state: () => {
          previd: string;
          inboundH: number;
          outboundH: number;
          maxResumeSeconds: number;
          unhandledOutboundEntries: XmppResumeEntry[];
        };
      };
    }).xmpp = {
      get_resume_state: () => ({
        previd: "live-sm-id",
        inboundH: 4,
        outboundH: 9,
        maxResumeSeconds: 300,
        unhandledOutboundEntries: [resumeEntry("message", "unacked")],
      }),
    };

    client.persistResumeStateForPageHide();
    await flushMicrotasks();

    expect(persistence.smSnapshot()).toMatchObject({
      previd: "live-sm-id",
      inboundH: 4,
      outboundH: 9,
      maxResumeSeconds: 300,
      unhandledOutboundEntries: [resumeEntry("message", "unacked")],
    });
  });

  test("BrowserXmppClient treats restored SM message stanzas as inflight queued sends", async () => {
    const persistence = inMemoryPersistence();
    const durableOutboundStore = new MemoryDurableOutboundStore();
    await committed(persistence.saveSm({
      previd: "live-sm-id",
      inboundH: 4,
      outboundH: 9,
      unhandledOutboundEntries: [resumeEntry("message", "dm-live-1")],
    }));
    const queued = {
      kind: "dm",
      id: "dm-live-1",
      createdAt: new Date().toISOString(),
      peerJid: "bob@example.com",
      body: "hello",
    } as const;
    await committed(durableOutboundStore.persistReady("alice@example.com", queued));
    enqueueQueuedMessage("alice@example.com", queued);

    const client = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      mockPersistenceOptions(persistence, durableOutboundStore),
    );
    await waitForClientStartup(client);

    const queue = (client as unknown as {
      outboundQueue: {
        beginConnectionGeneration(generation: number): number;
        reconcileNativeSnapshot(
          generation: number,
          state: PersistedSmResumeState,
        ): Promise<unknown>;
      };
    }).outboundQueue;
    queue.beginConnectionGeneration(1);
    (client as unknown as { connectEpoch: number }).connectEpoch = 1;
    await queue.reconcileNativeSnapshot(1, {
      previd: "live-sm-id",
      inboundH: 4,
      outboundH: 9,
      unhandledOutboundEntries: [resumeEntry("message", "dm-live-1")],
    });
    (client as unknown as { handleMessageAck: (id: string, generation: number) => void })
      .handleMessageAck("dm-live-1", 1);
    await flushMicrotasks();

    expect(listQueuedDmMessages("alice@example.com", "bob@example.com", "account")).toEqual([]);
  });

  test("BrowserXmppClient retains restored SM queue entries while native fallback retry owns resend", async () => {
    const persistence = inMemoryPersistence();
    const durableOutboundStore = new MemoryDurableOutboundStore();
    await committed(persistence.saveSm({
      previd: "live-sm-id",
      inboundH: 4,
      outboundH: 9,
      unhandledOutboundEntries: [resumeEntry("message", "dm-live-1")],
    }));
    const queued = {
      kind: "dm",
      id: "dm-live-1",
      createdAt: new Date().toISOString(),
      peerJid: "bob@example.com",
      body: "hello",
    } as const;
    await committed(durableOutboundStore.persistReady("alice@example.com", queued));
    enqueueQueuedMessage("alice@example.com", queued);

    const client = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      mockPersistenceOptions(persistence, durableOutboundStore),
    );
    await waitForClientStartup(client);

    (client as unknown as { handleMessageFailed: (id: string) => void }).handleMessageFailed("dm-live-1");
    await flushMicrotasks();

    expect(listQueuedDmMessages("alice@example.com", "bob@example.com", "account").map((entry) => entry.id))
      .toEqual(["dm-live-1"]);
  });

  test("BrowserXmppClient persists SM state when the native replay queue is empty", async () => {
    const persistence = inMemoryPersistence();
    const client = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      mockPersistenceOptions(persistence),
    );
    await waitForClientStartup(client);
    (client as unknown as {
      xmpp: { get_resume_state: () => PersistedSmResumeState };
    }).xmpp = {
      get_resume_state: () => ({
        previd: "live-sm-id",
        inboundH: 4,
        outboundH: 9,
        maxResumeSeconds: 300,
        unhandledOutboundEntries: [],
      }),
    };

    client.persistResumeStateForPageHide();
    await flushMicrotasks();

    expect(persistence.smSnapshot()).toMatchObject({
      previd: "live-sm-id",
      inboundH: 4,
      outboundH: 9,
      maxResumeSeconds: 300,
    });
  });

  test("round-trips retained joined rooms for refresh-time group call discovery", async () => {
    const store = new MemoryDurableOutboundStore();
    const persistence = createLocalStorageResumePersistence("alice@example.com", "tab-a", store);
    const otherTab = createLocalStorageResumePersistence("alice@example.com", "tab-b", store);
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

  test("consumeSm rejects a delayed claimant that observed the same original resource", async () => {
    const store = new MemoryDurableSmResumeStore();
    const lifecycleId = XmppLifecycleId.explicitForTest("delayed-claimant");
    const first = createLocalStorageResumePersistence(
      "alice@example.com",
      "tab-a",
      store,
      lifecycleId,
    );
    const second = createLocalStorageResumePersistence(
      "alice@example.com",
      "tab-a",
      store,
      lifecycleId,
    );
    const state: PersistedSmResumeState = {
      previd: "abc-123",
      inboundH: 42,
      outboundH: 7,
      maxResumeSeconds: 300,
      unhandledOutboundEntries: [],
      resource: "web-existing-resource",
    };
    await committed(first.saveSm(state));

    const results = await Promise.all([first.consumeSm(), second.consumeSm()]);
    const committedValues = results
      .filter((result) => result.kind === "committed")
      .map((result) => result.value)
      .filter((result) => result !== null);
    expect(committedValues).toEqual([state]);
    expect(results.filter((result) => result.kind === "failed")).toHaveLength(1);
    expect(await committed(first.loadSm())).toBeNull();
    expect(await committed(first.consumeSm())).toBeNull();
  });

  test("a consumed snapshot cannot become reusable when the replacement save aborts", async () => {
    const store = new MemoryDurableSmResumeStore();
    const persistence = createLocalStorageResumePersistence("alice@example.com", "tab-a", store);
    const original: PersistedSmResumeState = {
      previd: "s0",
      inboundH: 7,
      outboundH: 9,
      unhandledOutboundEntries: [],
    };
    await committed(persistence.saveSm(original));
    expect(await committed(persistence.consumeSm())).toEqual(original);

    store.saveSm = async () => ({
      kind: "failed",
      reason: "aborted",
      cause: new DOMException("replacement aborted", "AbortError"),
    });
    const replacement = await persistence.saveSm({
      previd: "s1",
      inboundH: 8,
      outboundH: 10,
      unhandledOutboundEntries: [],
    });

    expect(replacement.kind).toBe("failed");
    expect(await committed(persistence.loadSm())).toBeNull();
    expect(await committed(persistence.consumeSm())).toBeNull();
  });

  test("SM TTL: an old envelope without advertised max uses the default resume window", async () => {
    const store = new MemoryDurableSmResumeStore();
    const persistence = createLocalStorageResumePersistence("alice@example.com", "tab-a", store);
    const justOverDefaultResumeWindow = Date.now() - 301_000;
    await seedRawSm(
      store,
      "tab-a",
      {
        previd: "stale",
        inboundH: 1,
        outboundH: 1,
        unhandledOutboundEntries: [],
      },
      justOverDefaultResumeWindow,
    );
    expect(await committed(persistence.loadSm())).toBeNull();
  });

  test("SM TTL: advertised maxResumeSeconds controls persisted resume expiry", async () => {
    const store = new MemoryDurableSmResumeStore();
    const persistence = createLocalStorageResumePersistence("alice@example.com", "tab-a", store);
    const twoSecondsAgo = Date.now() - 2_000;
    await seedRawSm(
      store,
      "tab-a",
      {
        previd: "stale",
        inboundH: 1,
        outboundH: 1,
        maxResumeSeconds: 1,
        unhandledOutboundEntries: [],
      },
      twoSecondsAgo,
    );

    expect(await committed(persistence.consumeSm())).toBeNull();
  });

  test("SM TTL: future-dated envelopes fail closed", async () => {
    const store = new MemoryDurableSmResumeStore();
    const persistence = createLocalStorageResumePersistence("alice@example.com", "tab-a", store);
    const farFuture = Date.now() + 120_000;
    await seedRawSm(
      store,
      "tab-a",
      {
        previd: "from-the-future",
        inboundH: 1,
        outboundH: 1,
        maxResumeSeconds: 300,
        unhandledOutboundEntries: [],
      },
      farFuture,
    );

    expect(await committed(persistence.loadSm())).toBeNull();
  });

  test("SM state rejects non-u32 stanza counters", async () => {
    const store = new MemoryDurableSmResumeStore();
    await expectRawSmRejected(
      store,
      "tab-a",
      {
        previd: "bad-counter",
        inboundH: 1.5,
        outboundH: 1,
        unhandledOutboundEntries: [],
      } as PersistedSmResumeState,
      Date.now(),
    );
  });

  test("SM state rejects legacy raw XML entries and unknown structural fields", async () => {
    const store = new MemoryDurableSmResumeStore();
    const rawXml = {
      previd: "raw-xml",
      inboundH: 1,
      outboundH: 1,
      unhandledOutboundEntries: [{
        stanza: "<message xmlns='jabber:client'/>",
        sentAtEpochMs: RESUME_SENT_AT_EPOCH_MS,
      }],
    } as unknown as PersistedSmResumeState;
    await expectRawSmRejected(store, "tab-a", rawXml, Date.now());

    const unknown = resumeEntry("message", "unknown-field");
    (unknown.stanza.tokens[0] as unknown as Record<string, unknown>).stanzaXml = "<message/>";
    await expectRawSmRejected(store, "tab-a", {
      previd: "unknown-field",
      inboundH: 1,
      outboundH: 1,
      unhandledOutboundEntries: [unknown],
    }, Date.now());
  });

  test("SM state rejects unbalanced, over-depth, and oversized typed stanza tokens", async () => {
    const store = new MemoryDurableSmResumeStore();
    const candidates: PersistedSmResumeState[] = [];

    const unbalanced = resumeEntry("message", "unbalanced");
    unbalanced.stanza.tokens.pop();
    candidates.push({
      previd: "unbalanced",
      inboundH: 1,
      outboundH: 1,
      unhandledOutboundEntries: [unbalanced],
    });

    const tooDeep = resumeEntry("message", "too-deep");
    const end = tooDeep.stanza.tokens.pop()!;
    for (let depth = 0; depth < 64; depth += 1) {
      tooDeep.stanza.tokens.push({
        kind: "start",
        name: { namespace: "urn:waddle:test:resume", localName: "nested" },
        attributes: [],
      });
    }
    for (let depth = 0; depth < 64; depth += 1) tooDeep.stanza.tokens.push({ kind: "end" });
    tooDeep.stanza.tokens.push(end);
    candidates.push({
      previd: "too-deep",
      inboundH: 1,
      outboundH: 1,
      unhandledOutboundEntries: [tooDeep],
    });

    const oversized = resumeEntry("message", "oversized");
    const oversizedStart = oversized.stanza.tokens[0]!;
    const oversizedEnd = oversized.stanza.tokens[1]!;
    oversized.stanza.tokens = [
      oversizedStart,
      ...Array.from({ length: 16_383 }, () => ({
        kind: "text" as const,
        value: "",
      })),
      oversizedEnd,
    ];
    candidates.push({
      previd: "oversized",
      inboundH: 1,
      outboundH: 1,
      unhandledOutboundEntries: [oversized],
    });

    for (const candidate of candidates) {
      await expectRawSmRejected(store, "tab-a", candidate, Date.now());
    }
  });

  test("SM typed entry preserves exact safe-integer millisecond precision", async () => {
    const store = new MemoryDurableSmResumeStore();
    const persistence = createLocalStorageResumePersistence("alice@example.com", "tab-a", store);
    const entry = resumeEntry("message", "precise-time");
    entry.sentAtEpochMs = 1_748_779_140_123;
    const state: PersistedSmResumeState = {
      previd: "precise-time",
      inboundH: 1,
      outboundH: 1,
      unhandledOutboundEntries: [entry],
    };
    await committed(persistence.saveSm(state));
    expect((await committed(persistence.loadSm()))?.unhandledOutboundEntries?.[0]?.sentAtEpochMs)
      .toBe(1_748_779_140_123);
  });

  test("loadCatchup returns null for malformed JSON", async () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com", "tab-a");
    window.localStorage.setItem(catchupShardKey("alice@example.com", "tab-a"), "{not valid json");
    expect(persistence.loadCatchup()).toBeNull();
  });

  test("loadCatchup rejects payloads with the wrong shape", async () => {
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
    const store = new MemoryDurableOutboundStore();
    const persistenceA = createLocalStorageResumePersistence("alice@example.com", "tab-a", store);
    const persistenceB = createLocalStorageResumePersistence("alice@example.com", "tab-b", store);
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
    const shardPrefix = "waddle.chat.resume-cursors.17:alice@example.com.";
    const shardKeys = Array.from(
      { length: window.localStorage.length },
      (_, index) => window.localStorage.key(index),
    ).filter((key): key is string => key?.startsWith(shardPrefix) === true);
    expect(shardKeys).toHaveLength(2);
  });

  test("account shard prefixes cannot read or clear a longer JID", async () => {
    const store = new MemoryDurableOutboundStore();
    const alice = createLocalStorageResumePersistence("alice@example.com", "tab-a", store);
    const evil = createLocalStorageResumePersistence("alice@example.com.evil", "tab-b", store);
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
