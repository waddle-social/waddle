import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { effectScope, ref } from "vue";
import { useXmppRosterContacts } from "@/contacts/roster";
import { BrowserXmppClient } from "@/lib/xmpp/client";
import { TypedEventBus, type ClientEvents } from "@/lib/xmpp/client-events";
import { PresenceManager } from "@/lib/xmpp/client-presence";
import { nullResumePersistence } from "@/lib/xmpp/resume-persistence";
import type { RosterContact } from "@/lib/xmpp/types";
import { resetContactPresence, setContactPresence } from "@/presence/live-presence";

function installSessionStorage() {
  const store = new Map<string, string>();
  const originalWindow = globalThis.window;
  (globalThis as unknown as { window?: unknown }).window = {
    sessionStorage: {
      getItem: (key: string) => store.get(key) ?? null,
      setItem: (key: string, value: string) => { store.set(key, value); },
      removeItem: (key: string) => { store.delete(key); },
    },
  };
  return {
    store,
    restore() {
      if (originalWindow === undefined) delete (globalThis as unknown as { window?: unknown }).window;
      else globalThis.window = originalWindow;
    },
  };
}

describe("useXmppRosterContacts XEP-0237 cache and roster pushes", () => {
  let storageEnv: ReturnType<typeof installSessionStorage>;

  beforeEach(() => {
    storageEnv = installSessionStorage();
    resetContactPresence();
  });

  afterEach(() => {
    resetContactPresence();
    storageEnv.restore();
  });

  test("hydrates cached contacts immediately, revalidates with ver, and seeds live presence", async () => {
    const account = ref<string | null>("alice@example.com");
    storageEnv.store.set(
      "waddle.roster.v1:alice@example.com",
      JSON.stringify({
        ver: "v7",
        contacts: [
          { jid: "bob@example.com", name: "Bob", username: "Bob", subscription: "both", groups: ["Team"] },
        ],
      }),
    );
    setContactPresence("bob@example.com", "available");

    const listRosterContactsVersioned = mock(async (ver?: string | null) => ({
      unchanged: true,
      ver: ver ?? null,
      contacts: [] as RosterContact[],
    }));
    const client = ref({
      listRosterContacts: mock(async () => []),
      listRosterContactsVersioned,
    } as unknown as BrowserXmppClient | null);

    const scope = effectScope();
    const roster = scope.run(() => useXmppRosterContacts(client, account))!;
    try {
      expect(roster.contacts.value).toEqual([
        {
          jid: "bob@example.com",
          name: "Bob",
          username: "Bob",
          subscription: "both",
          groups: ["Team"],
          presenceShow: "available",
        },
      ]);

      await roster.loadRosterContacts();
      expect(listRosterContactsVersioned).toHaveBeenCalledTimes(1);
      expect(listRosterContactsVersioned).toHaveBeenCalledWith("v7");
      expect(roster.contacts.value).toHaveLength(1);
      expect(roster.contacts.value[0]?.presenceShow).toBe("available");
    } finally {
      scope.stop();
    }
  });

  test("deduplicates concurrent loadRosterContacts calls and merges roster pushes", async () => {
    const account = ref<string | null>("alice@example.com");
    let resolveLoad!: (res: { unchanged: boolean; ver: string | null; contacts: RosterContact[] }) => void;
    const listRosterContactsVersioned = mock(
      () => new Promise<{ unchanged: boolean; ver: string | null; contacts: RosterContact[] }>((resolve) => {
        resolveLoad = resolve;
      }),
    );
    const client = ref({
      listRosterContacts: mock(async () => []),
      listRosterContactsVersioned,
    } as unknown as BrowserXmppClient | null);

    const scope = effectScope();
    const roster = scope.run(() => useXmppRosterContacts(client, account))!;
    try {
      const p1 = roster.loadRosterContacts();
      const p2 = roster.loadRosterContacts();
      expect(listRosterContactsVersioned).toHaveBeenCalledTimes(1);

      resolveLoad({
        unchanged: false,
        ver: "v1",
        contacts: [
          { jid: "bob@example.com", name: "Bob", username: "Bob", subscription: "both", groups: [] },
        ],
      });
      await Promise.all([p1, p2]);

      expect(roster.contacts.value).toHaveLength(1);
      expect(JSON.parse(storageEnv.store.get("waddle.roster.v1:alice@example.com")!).ver).toBe("v1");

      // No-op presence updates must not replace the contacts array reference.
      const beforeMiss = roster.contacts.value;
      roster.updatePresence("stranger@example.com/web", "available");
      expect(roster.contacts.value).toBe(beforeMiss);

      roster.updatePresence("bob@example.com/web", "away");
      const afterAway = roster.contacts.value;
      expect(afterAway).not.toBe(beforeMiss);
      expect(afterAway[0]?.presenceShow).toBe("away");

      roster.updatePresence("bob@example.com/phone", "away");
      expect(roster.contacts.value).toBe(afterAway);

      // Push adds Carol and updates ver.
      setContactPresence("carol@example.com", "dnd");
      roster.applyRosterPush({
        ver: "v2",
        items: [
          { jid: "carol@example.com", name: "Carol", username: "Carol", subscription: "to", groups: ["Ops"] },
        ],
      });
      expect(roster.contacts.value.map((c) => c.jid)).toEqual(["bob@example.com", "carol@example.com"]);
      expect(roster.contacts.value[1]?.presenceShow).toBe("dnd");
      expect(JSON.parse(storageEnv.store.get("waddle.roster.v1:alice@example.com")!).ver).toBe("v2");

      // Push with subscription="remove" drops Bob and updates ver.
      roster.applyRosterPush({
        ver: "v3",
        items: [
          { jid: "bob@example.com", username: "bob", subscription: "remove", groups: [] },
        ],
      });
      expect(roster.contacts.value.map((c) => c.jid)).toEqual(["carol@example.com"]);
      expect(JSON.parse(storageEnv.store.get("waddle.roster.v1:alice@example.com")!).ver).toBe("v3");

      // Logout clears persisted cache.
      roster.clearRosterContacts({ clearPersistedCache: true });
      expect(roster.contacts.value).toEqual([]);
      expect(storageEnv.store.has("waddle.roster.v1:alice@example.com")).toBe(false);
    } finally {
      scope.stop();
    }
  });

  test("does not stamp rosterVersion or persist a partial cache on pushes before initial full load", async () => {
    const listRosterContactsVersioned = mock(async (_ver?: string | null) => ({
      unchanged: false,
      ver: "v10",
      contacts: [
        { jid: "bob@example.com", name: "Bob", username: "Bob", subscription: "both" as const, groups: [] },
        { jid: "carol@example.com", name: "Carol", username: "Carol", subscription: "to" as const, groups: [] },
      ],
    }));

    const clientRef = ref({
      listRosterContactsVersioned,
    } as unknown as BrowserXmppClient);
    const accountRef = ref<string | null>("alice@example.com");

    const scope = effectScope();
    const roster = scope.run(() => useXmppRosterContacts(clientRef, accountRef))!;
    try {
      // A push arrives before any full roster snapshot has been loaded.
      roster.applyRosterPush({
        ver: "v9",
        items: [
          { jid: "carol@example.com", name: "Carol", username: "Carol", subscription: "to", groups: [] },
        ],
      });
      expect(roster.contacts.value.map((c) => c.jid)).toEqual(["carol@example.com"]);
      expect(storageEnv.store.has("waddle.roster.v1:alice@example.com")).toBe(false);

      // Subsequent load MUST send ver=null (not "v9") to fetch the complete roster.
      await roster.loadRosterContacts();
      expect(listRosterContactsVersioned).toHaveBeenCalledWith(null);
      expect(roster.contacts.value.map((c) => c.jid)).toEqual(["bob@example.com", "carol@example.com"]);
      expect(JSON.parse(storageEnv.store.get("waddle.roster.v1:alice@example.com")!).ver).toBe("v10");
    } finally {
      scope.stop();
    }
  });
});

describe("BrowserXmppClient fetchInbox in-flight deduplication", () => {
  test("shares one wire fetch_inbox call across concurrent callers with matching options", async () => {
    const client = new BrowserXmppClient(
      {
        username: "alice",
        jid: "alice@example.com",
        session_id: "sess-1",
        xmpp_websocket_url: "wss://example.com/ws",
      },
      nullResumePersistence,
    );
    let resolveInbox!: (value: { total_unread: number; total: number; unread_conversations: number; conversations: [] }) => void;
    const fetch_inbox = mock(
      () => new Promise<{ total_unread: number; total: number; unread_conversations: number; conversations: [] }>((resolve) => {
        resolveInbox = resolve;
      }),
    );
    const fakeXmpp = { fetch_inbox };
    (client as unknown as { requireConnectedXmpp: () => Promise<typeof fakeXmpp> }).requireConnectedXmpp = async () => fakeXmpp;

    const first = client.fetchInbox();
    const second = client.fetchInbox({});
    await Promise.resolve();
    expect(fetch_inbox).toHaveBeenCalledTimes(1);

    resolveInbox({ total_unread: 3, total: 1, unread_conversations: 1, conversations: [] });
    const [r1, r2] = await Promise.all([first, second]);
    expect(r1).toEqual(r2);
    expect(r1.totalUnread).toBe(3);

    // After resolution, a fresh fetchInbox fires again.
    fetch_inbox.mockImplementationOnce(async () => ({
      total_unread: 0,
      total: 0,
      unread_conversations: 0,
      conversations: [],
    }));
    const r3 = await client.fetchInbox();
    expect(fetch_inbox).toHaveBeenCalledTimes(2);
    expect(r3.totalUnread).toBe(0);
  });

  test("clears in-flight entry cleanly on rejection without unhandled rejection", async () => {
    const client = new BrowserXmppClient(
      {
        username: "alice",
        jid: "alice@example.com",
        session_id: "sess-1",
        xmpp_websocket_url: "wss://example.com/ws",
      },
      nullResumePersistence,
    );
    const fetch_inbox = mock(async () => {
      throw new Error("timeout");
    });
    const fakeXmpp = { fetch_inbox };
    (client as unknown as { requireConnectedXmpp: () => Promise<typeof fakeXmpp> }).requireConnectedXmpp = async () => fakeXmpp;

    await expect(client.fetchInbox()).rejects.toThrow("timeout");
    // Entry was removed on rejection so retry issues a fresh wire call.
    fetch_inbox.mockImplementationOnce(async () => ({
      total_unread: 1,
      total: 1,
      unread_conversations: 1,
      conversations: [],
    }));
    const retried = await client.fetchInbox();
    expect(fetch_inbox).toHaveBeenCalledTimes(2);
    expect(retried.totalUnread).toBe(1);
  });
});

describe("PresenceManager incremental occupant index", () => {
  test("updates affected bare JIDs incrementally and suppresses no-op occupantPresence emits", () => {
    const events = new TypedEventBus<ClientEvents>();
    const occupantSnapshots: Array<Record<string, string[]>> = [];
    const roomPresences: Array<Record<string, string>> = [];
    events.on("occupantPresence", (snapshot) => {
      occupantSnapshots.push(snapshot);
    });
    events.set("presence", (presence) => {
      roomPresences.push({ ...presence });
    });

    const manager = new PresenceManager({
      events,
      currentRoom: () => "room1@muc.example.com",
      isKnownMucRoom: (bare) => bare.endsWith("@muc.example.com"),
      ownFullJidCandidates: () => new Set(["alice@example.com/web"]),
      requireConnectedXmpp: async () => ({}),
      handleMucPresenceError: () => false,
      onOwnSelfPresence: () => {},
      onOwnUnavailable: () => {},
    });

    // Bob joins room1 -> emits once.
    manager.handle({
      from: "room1@muc.example.com/bob",
      muc_jid: "bob@example.com/desktop",
      muc_affiliation: "member",
      muc_role: "participant",
    });
    expect(occupantSnapshots).toHaveLength(1);
    expect(occupantSnapshots[0]).toEqual({ "bob@example.com": ["online"] });
    expect(roomPresences).toHaveLength(1);

    // Identical re-broadcast -> suppressed (no duplicate occupantPresence or room presence emit).
    manager.handle({
      from: "room1@muc.example.com/bob",
      muc_jid: "bob@example.com/desktop",
      muc_affiliation: "member",
      muc_role: "participant",
    });
    expect(occupantSnapshots).toHaveLength(1);
    expect(roomPresences).toHaveLength(1);

    // Bob also joins room2 as away -> aggregate includes both shows.
    manager.handle({
      from: "room2@muc.example.com/bob",
      show: "away",
      muc_jid: "bob@example.com/mobile",
      muc_affiliation: "member",
      muc_role: "participant",
    });
    expect(occupantSnapshots).toHaveLength(2);
    expect(occupantSnapshots[1]?.["bob@example.com"]?.sort()).toEqual(["away", "online"]);

    // Bob leaves room1 -> only room2's away remains.
    manager.handle({
      from: "room1@muc.example.com/bob",
      presence_type: "unavailable",
      muc_jid: "bob@example.com/desktop",
      muc_affiliation: "member",
      muc_role: "none",
    });
    expect(occupantSnapshots).toHaveLength(3);
    expect(occupantSnapshots[2]).toEqual({ "bob@example.com": ["away"] });

    // Bob leaves room2 -> entry removed from snapshot.
    manager.handle({
      from: "room2@muc.example.com/bob",
      presence_type: "unavailable",
      muc_jid: "bob@example.com/mobile",
      muc_affiliation: "member",
      muc_role: "none",
    });
    expect(occupantSnapshots).toHaveLength(4);
    expect(occupantSnapshots[3]).toEqual({});
  });

  test("emits occupantPresence when leaving a solo room while another room still has occupants", () => {
    const events = new TypedEventBus<ClientEvents>();
    const occupantSnapshots: Array<Record<string, string[]>> = [];
    events.on("occupantPresence", (snapshot) => {
      occupantSnapshots.push(snapshot);
    });

    const manager = new PresenceManager({
      events,
      currentRoom: () => "solo@muc.example.com",
      isKnownMucRoom: (bare) => bare.endsWith("@muc.example.com"),
      ownFullJidCandidates: () => new Set(["alice@example.com/web"]),
      requireConnectedXmpp: async () => ({}),
      handleMucPresenceError: () => false,
      onOwnSelfPresence: () => {},
      onOwnUnavailable: () => {},
    });

    manager.handle({
      from: "other@muc.example.com/bob",
      muc_jid: "bob@example.com/desktop",
      muc_affiliation: "member",
      muc_role: "participant",
    });
    manager.handle({
      from: "solo@muc.example.com/alice",
      muc_jid: "alice@example.com/web",
      muc_affiliation: "owner",
      muc_role: "moderator",
      muc_status_codes: [110],
    });
    expect(occupantSnapshots.at(-1)).toEqual({
      "bob@example.com": ["online"],
      "alice@example.com": ["online"],
    });

    // Leaving solo@muc.example.com must emit a snapshot without alice while keeping bob.
    manager.handle({
      from: "solo@muc.example.com/alice",
      presence_type: "unavailable",
      muc_jid: "alice@example.com/web",
      muc_affiliation: "owner",
      muc_role: "none",
      muc_status_codes: [110],
    });
    expect(occupantSnapshots.at(-1)).toEqual({
      "bob@example.com": ["online"],
    });
  });
});
