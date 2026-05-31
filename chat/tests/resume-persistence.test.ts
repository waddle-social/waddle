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
import { enqueueQueuedMessage, listQueuedDmMessages } from "../src/lib/outbound-queue-store";
import type { WaddleSession } from "../src/lib/server-auth";
import {
  createLocalStorageResumePersistence,
  nullResumePersistence,
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
  return {
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
    preparePagehideHandoff: () => undefined,
    loadJoinedRooms: () => [...joinedRooms],
    saveJoinedRooms: (rooms) => { joinedRooms = [...rooms]; },
    clearJoinedRooms: () => { joinedRooms = []; },
    catchupSnapshot: () => catchup,
    smSnapshot: () => sm,
    joinedRoomsSnapshot: () => [...joinedRooms],
    clearSmCount: () => smClears,
  };
}

async function flushMicrotasks() {
  // `queueMicrotask` callbacks fire after the current sync block but
  // before the next macrotask. A zero-delay setTimeout drains them.
  await new Promise((resolve) => setTimeout(resolve, 0));
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

  test("hydrated instance returns cursors on the *first* onSessionStarted (PR3 whole point)", () => {
    // A hydrated `ReconnectCatchup` represents a prior tab session
    // that left cursors behind in localStorage. The next page load
    // gets a fresh instance, but the very first `session:started`
    // after that load must return the hydrated cursors so MAM
    // catch-up runs immediately — otherwise the persistence is
    // dead weight (writes on every advance, never consumed).
    const persistence = inMemoryPersistence();
    persistence.saveCatchup({
      dmLastSeen: [["bob@example.com", { timestamp: "2026-05-20T10:00:00.000Z", archiveId: "dm-arch-1" }]],
      roomLastSeen: [],
    });

    const c = new ReconnectCatchup(persistence);
    const entries = c.onSessionStarted();
    expect(entries).toEqual([
      { kind: "dm", key: "bob@example.com", after: "dm-arch-1", since: "2026-05-20T10:00:00.000Z" },
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
        archiveId: "dm-arch-1",
        archiveTimestamp: "2026-05-20T10:00:00.000Z",
      },
    ]);
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

describe("createLocalStorageResumePersistence — localStorage adapter", () => {
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
      unhandledOutboundStanzas: ["<message xmlns='jabber:client' id='m1'/>"],
    };
    persistence.saveSm(state);
    // Round-trip strips the internal `savedAt` so the caller gets
    // the same shape it passed in.
    expect(persistence.loadSm()).toEqual(state);
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
      `waddle.chat.sm-resume.owner-lease.${copiedOwner}`,
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

  test("a pagehide handoff keeps the copied owner for same-tab reload consumption", () => {
    const reloadOwner = "reload-owner";
    const state = {
      previd: "abc-123",
      inboundH: 42,
      outboundH: 7,
      resource: "web-existing-resource",
    };
    const previousPage = createLocalStorageResumePersistence("alice@example.com", reloadOwner);
    previousPage.saveSm(state);
    previousPage.preparePagehideHandoff();
    window.sessionStorage.setItem("waddle.chat.sm-resume.owner", reloadOwner);
    window.localStorage.setItem(
      `waddle.chat.sm-resume.owner-lease.${reloadOwner}`,
      JSON.stringify({
        ownerId: reloadOwner,
        instanceId: "previous-page",
        updatedAt: Date.now(),
      }),
    );

    const reloadedPage = createLocalStorageResumePersistence("alice@example.com");

    expect(reloadedPage.consumeSm()).toEqual(state);
  });

  test("a slow same-tab reload keeps the owner until the prior lease expires", () => {
    const realNow = Date.now;
    let now = 1_000_000;
    Date.now = () => now;

    try {
      const reloadOwner = "slow-reload-owner";
      const state = {
        previd: "abc-123",
        inboundH: 42,
        outboundH: 7,
        resource: "web-existing-resource",
      };
      const previousPage = createLocalStorageResumePersistence("alice@example.com", reloadOwner);
      previousPage.saveSm(state);
      previousPage.saveJoinedRooms(["general@conference.example.com"]);
      previousPage.preparePagehideHandoff();
      window.sessionStorage.setItem("waddle.chat.sm-resume.owner", reloadOwner);
      window.localStorage.setItem(
        `waddle.chat.sm-resume.owner-lease.${reloadOwner}`,
        JSON.stringify({
          ownerId: reloadOwner,
          instanceId: "previous-page",
          updatedAt: now,
        }),
      );

      now += 15_000;
      const reloadedPage = createLocalStorageResumePersistence("alice@example.com");

      expect(reloadedPage.loadJoinedRooms()).toEqual(["general@conference.example.com"]);
      expect(reloadedPage.consumeSm()).toEqual(state);
    } finally {
      Date.now = realNow;
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

  test("BrowserXmppClient does not persist lossy SM state while outbound stanzas are unacked", () => {
    const persistence = inMemoryPersistence();
    persistence.saveJoinedRooms(["general@conference.example.com"]);
    const consumedClearCount = persistence.clearSmCount();
    const client = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      persistence,
    );
    (client as unknown as {
      xmpp: { get_resume_state: () => { previd: string; inboundH: number; outboundH: number; hasUnackedOutbound: boolean } };
    }).xmpp = {
      get_resume_state: () => ({
        previd: "live-sm-id",
        inboundH: 4,
        outboundH: 9,
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
          hasUnackedOutbound: boolean;
          unhandledOutboundStanzas: string[];
        };
      };
    }).xmpp = {
      get_resume_state: () => ({
        previd: "live-sm-id",
        inboundH: 4,
        outboundH: 9,
        hasUnackedOutbound: true,
        unhandledOutboundStanzas: ["<message xmlns='jabber:client' id='unacked'/>"],
      }),
    };

    client.persistResumeStateForPageHide();

    expect(persistence.smSnapshot()).toMatchObject({
      previd: "live-sm-id",
      inboundH: 4,
      outboundH: 9,
      unhandledOutboundStanzas: ["<message xmlns='jabber:client' id='unacked'/>"],
    });
  });

  test("BrowserXmppClient treats restored SM message stanzas as inflight queued sends", () => {
    const persistence = inMemoryPersistence();
    persistence.saveSm({
      previd: "live-sm-id",
      inboundH: 4,
      outboundH: 9,
      unhandledOutboundStanzas: ["<message xmlns='jabber:client' id='dm-live-1'/>"],
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

    expect(listQueuedDmMessages("alice@example.com", "bob@example.com")).toEqual([]);
  });

  test("BrowserXmppClient removes restored SM queue entries when native fallback retry owns resend", () => {
    const persistence = inMemoryPersistence();
    persistence.saveSm({
      previd: "live-sm-id",
      inboundH: 4,
      outboundH: 9,
      unhandledOutboundStanzas: ["<message xmlns='jabber:client' id='dm-live-1'/>"],
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

    expect(listQueuedDmMessages("alice@example.com", "bob@example.com")).toEqual([]);
  });

  test("BrowserXmppClient persists SM state when the native replay queue is empty", () => {
    const persistence = inMemoryPersistence();
    const client = new BrowserXmppClient(
      { jid: "alice@example.com", username: "alice" } as WaddleSession,
      persistence,
    );
    (client as unknown as {
      xmpp: { get_resume_state: () => { previd: string; inboundH: number; outboundH: number; hasUnackedOutbound: boolean } };
    }).xmpp = {
      get_resume_state: () => ({
        previd: "live-sm-id",
        inboundH: 4,
        outboundH: 9,
        hasUnackedOutbound: false,
      }),
    };

    client.persistResumeStateForPageHide();

    expect(persistence.smSnapshot()).toMatchObject({
      previd: "live-sm-id",
      inboundH: 4,
      outboundH: 9,
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

  test("consumeSm rejects a delayed claimant that observed the same original resource", () => {
    const first = createLocalStorageResumePersistence("alice@example.com");
    const second = createLocalStorageResumePersistence("alice@example.com");
    const state: PersistedSmResumeState = {
      previd: "abc-123",
      inboundH: 42,
      outboundH: 7,
      resource: "web-existing-resource",
    };
    first.saveSm(state);

    const storage = window.localStorage;
    const originalSetItem = storage.setItem.bind(storage);
    let reentered = false;
    let secondResult: PersistedSmResumeState | null | undefined;
    storage.setItem = ((key: string, value: string) => {
      if (
        key === "waddle.chat.sm-resume.alice@example.com"
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

  test("SM TTL: a stale envelope is rejected and cleared", () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com");
    // Inject a 30-day-old envelope directly (bypassing saveSm so we
    // control the timestamp). 30d > the 24h TTL.
    const thirtyDaysAgo = Date.now() - (30 * 24 * 60 * 60 * 1000);
    window.localStorage.setItem(
      "waddle.chat.sm-resume.alice@example.com",
      JSON.stringify({ previd: "stale", inboundH: 1, outboundH: 1, savedAt: thirtyDaysAgo }),
    );
    expect(persistence.loadSm()).toBeNull();
    // Stale entries are pruned on read so the next load doesn't
    // pay the validation cost again.
    expect(window.localStorage.getItem("waddle.chat.sm-resume.alice@example.com")).toBeNull();
  });

  test("loadCatchup returns null for malformed JSON", () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com");
    window.localStorage.setItem("waddle.chat.resume-cursors.alice@example.com", "{not valid json");
    expect(persistence.loadCatchup()).toBeNull();
  });

  test("loadCatchup rejects payloads with the wrong shape", () => {
    const persistence = createLocalStorageResumePersistence("alice@example.com");
    window.localStorage.setItem(
      "waddle.chat.resume-cursors.alice@example.com",
      JSON.stringify({ dmLastSeen: "not-an-array", roomLastSeen: [] }),
    );
    expect(persistence.loadCatchup()).toBeNull();
  });

  test("two tabs converge: read-merge-write avoids cursor clobber", async () => {
    // Simulate two `ReconnectCatchup` instances sharing the same
    // localStorage (two tabs of the same account). Each advances
    // its own conversation; after both persist, BOTH conversations
    // are present in storage — the pre-fix behavior would have
    // overwritten one with the other.
    const persistence = createLocalStorageResumePersistence("alice@example.com");
    const tabA = new ReconnectCatchup(persistence);
    const tabB = new ReconnectCatchup(persistence);
    tabA.recordDmSeen("bob@example.com", "2026-05-20T10:00:00Z", "dm-arch-A");
    tabB.recordRoomSeen("general@muc.example.com", "2026-05-20T11:00:00Z", "room-arch-B");
    await new Promise((resolve) => setTimeout(resolve, 0));
    const final = persistence.loadCatchup();
    expect(final).not.toBeNull();
    expect(final!.dmLastSeen.map((e) => e[0])).toContain("bob@example.com");
    expect(final!.roomLastSeen.map((e) => e[0])).toContain("general@muc.example.com");
  });
});
