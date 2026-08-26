import { describe, test, expect, mock } from "bun:test";
import { nextTick, ref } from "vue";
import { useDirectMessageConversations } from "../src/dms/conversations";
import type { WaddleSession } from "../src/lib/server-auth";
import type { BrowserXmppClient, InboxEntry, LiveDmMessage } from "../src/lib/xmpp-client";

function makeSession(jid = "alice@example.com/web"): WaddleSession {
  return { jid, username: "alice", token: "tok" } as WaddleSession;
}

type MockClient = BrowserXmppClient & {
  fetchInbox: ReturnType<typeof mock>;
  markInboxRead: ReturnType<typeof mock>;
  subscribeToPeerPresence: ReturnType<typeof mock>;
  hydrateRecentDmCallActivity: ReturnType<typeof mock>;
  hydrateRecentDmCallActivities: ReturnType<typeof mock>;
};

function makeClient(conversations: InboxEntry[] = []): MockClient {
  return {
    fetchInbox: mock(() => Promise.resolve({ totalUnread: conversations.reduce((sum, conversation) => sum + conversation.unread, 0), conversations })),
    markInboxRead: mock(() => Promise.resolve()),
    subscribeToPeerPresence: mock(() => Promise.resolve()),
    hydrateRecentDmCallActivity: mock(() => Promise.resolve()),
    hydrateRecentDmCallActivities: mock(() => Promise.resolve()),
  } as unknown as MockClient;
}

function makeComposable(
  { jid = "alice@example.com/web", client = null }: { jid?: string; client?: MockClient | null } = {},
) {
  const session = ref<WaddleSession | null>(makeSession(jid));
  const clientRef = ref<BrowserXmppClient | null>(client);
  return { composable: useDirectMessageConversations(session, clientRef), session, client: clientRef };
}

function makeDmMessage(overrides: Partial<LiveDmMessage> = {}): LiveDmMessage {
  return {
    id: "msg-1",
    peerJid: "bob@example.com",
    fromJid: "bob@example.com",
    nick: "bob",
    body: "hello",
    createdAt: new Date().toISOString(),
    type: "message",
    ...overrides,
  };
}

function epochSeconds(value: string): number {
  return Math.floor(new Date(value).getTime() / 1000);
}

describe("useDirectMessageConversations", () => {
  test("starts with empty state", () => {
    const { composable } = makeComposable();
    expect(composable.conversations.value).toEqual([]);
    expect(composable.activePeerJid.value).toBeNull();
    expect(composable.hasUnread.value).toBe(false);
    expect(composable.totalUnreadCount.value).toBe(0);
  });

  test("openDm creates a conversation and sets activePeerJid", async () => {
    const { composable } = makeComposable();
    await composable.openDm("bob@example.com");
    expect(composable.activePeerJid.value).toBe("bob@example.com");
    expect(composable.conversations.value).toHaveLength(1);
    expect(composable.conversations.value[0].peerUsername).toBe("bob");
  });

  test("openDm strips resource from JID", async () => {
    const { composable } = makeComposable();
    await composable.openDm("bob@example.com/mobile");
    expect(composable.activePeerJid.value).toBe("bob@example.com");
    expect(composable.conversations.value[0].peerJid).toBe("bob@example.com");
  });

  test("openDm does not duplicate conversations", async () => {
    const { composable } = makeComposable();
    await composable.openDm("bob@example.com");
    await composable.openDm("bob@example.com");
    expect(composable.conversations.value).toHaveLength(1);
  });

  test("forgetPeer drops a conversation and clears it when active", async () => {
    const { composable } = makeComposable();
    await composable.openDm("chat@example.com");
    expect(composable.conversations.value).toHaveLength(1);
    composable.forgetPeer("chat@example.com");
    expect(composable.conversations.value).toHaveLength(0);
    expect(composable.activePeerJid.value).toBeNull();
  });

  test("openDm refreshes per-peer call activity for hard-reload recovery", async () => {
    const client = makeClient();
    const { composable } = makeComposable({ client });

    await composable.openDm("bob@example.com/mobile");

    expect(client.hydrateRecentDmCallActivity).toHaveBeenCalledTimes(1);
    expect(client.hydrateRecentDmCallActivity).toHaveBeenCalledWith("bob@example.com");
    expect(client.subscribeToPeerPresence).toHaveBeenCalledWith("bob@example.com");
  });

  test("restored active DM refreshes per-peer call activity when the client attaches", async () => {
    const client = makeClient();
    const { composable, client: clientRef } = makeComposable();

    await composable.openDm("bob@example.com");
    clientRef.value = client;
    await nextTick();

    expect(client.hydrateRecentDmCallActivity).toHaveBeenCalledTimes(1);
    expect(client.hydrateRecentDmCallActivity).toHaveBeenCalledWith("bob@example.com");
  });

  test("active DM refreshes per-peer call activity again after client rotation", async () => {
    const firstClient = makeClient();
    const secondClient = makeClient();
    const { composable, client: clientRef } = makeComposable({ client: firstClient });

    await composable.openDm("bob@example.com");
    clientRef.value = secondClient;
    await nextTick();

    expect(firstClient.hydrateRecentDmCallActivity).toHaveBeenCalledTimes(1);
    expect(secondClient.hydrateRecentDmCallActivity).toHaveBeenCalledTimes(1);
    expect(secondClient.hydrateRecentDmCallActivity).toHaveBeenCalledWith("bob@example.com");
  });

  test("closeDm clears activePeerJid", async () => {
    const { composable } = makeComposable();
    await composable.openDm("bob@example.com");
    composable.closeDm();
    expect(composable.activePeerJid.value).toBeNull();
  });

  test("hydrateFromInbox merges direct conversations into the DM list", async () => {
    const client = makeClient([
      {
        partner: "bob@example.com",
        kind: "direct",
        lastStanzaId: "sid-1",
        lastUpdated: epochSeconds("2026-01-02T12:00:00Z"),
        unread: 2,
        preview: "from inbox",
      },
      {
        partner: "general@conference.example.com",
        kind: "muc",
        lastStanzaId: "sid-2",
        lastUpdated: epochSeconds("2026-01-02T12:05:00Z"),
        unread: 4,
        preview: "ignore me",
      },
    ]);
    const { composable } = makeComposable({ client });

    await composable.hydrateFromInbox();

    expect(composable.conversations.value).toHaveLength(1);
    expect(composable.conversations.value[0]).toMatchObject({
      peerJid: "bob@example.com",
      peerUsername: "bob",
      lastMessageBody: "from inbox",
      unreadCount: 2,
    });
    expect(composable.totalUnreadCount.value).toBe(2);
    expect(client.subscribeToPeerPresence).toHaveBeenCalledWith("bob@example.com");
  });

  test("hydrateFromInbox preserves newer live DM activity", async () => {
    const client = makeClient([
      {
        partner: "bob@example.com",
        kind: "direct",
        lastStanzaId: "sid-1",
        lastUpdated: epochSeconds("2026-01-01T00:00:00Z"),
        unread: 0,
        preview: "older preview",
      },
    ]);
    const { composable } = makeComposable({ client });

    composable.receiveIncomingDm(makeDmMessage({ createdAt: "2026-01-02T00:00:00Z", body: "newer live message" }));
    await composable.hydrateFromInbox();

    expect(composable.conversations.value[0]).toMatchObject({
      lastMessageBody: "newer live message",
      lastMessageAt: "2026-01-02T00:00:00Z",
      unreadCount: 1,
    });
  });

  test("hydrateFromInbox refreshes DM call activity from personal MAM", async () => {
    const client = makeClient([
      {
        partner: "bob@example.com",
        kind: "direct",
        lastStanzaId: "sid-1",
        lastUpdated: epochSeconds("2026-01-02T12:00:00Z"),
        unread: 0,
        preview: "from inbox",
      },
      {
        partner: "carol@example.com/mobile",
        kind: "direct",
        lastStanzaId: "sid-2",
        lastUpdated: epochSeconds("2026-01-02T12:05:00Z"),
        unread: 0,
        preview: "from inbox",
      },
    ]);
    const { composable } = makeComposable({ client });

    await composable.hydrateFromInbox();

    expect(client.hydrateRecentDmCallActivities).toHaveBeenCalledTimes(1);
  });

  test("hydrateFromInbox keeps inbox results when DM call hydration fails", async () => {
    const client = makeClient([
      {
        partner: "bob@example.com",
        kind: "direct",
        lastStanzaId: "sid-1",
        lastUpdated: epochSeconds("2026-01-02T12:00:00Z"),
        unread: 0,
        preview: "from inbox",
      },
    ]);
    client.hydrateRecentDmCallActivities = mock(async () => {
      throw new Error("MAM unavailable");
    });
    const { composable } = makeComposable({ client });

    const hydrated = await composable.hydrateFromInbox();

    expect(hydrated).toBe(true);
    expect(composable.conversations.value).toHaveLength(1);
    expect(client.hydrateRecentDmCallActivities).toHaveBeenCalledTimes(1);
  });

  test("hydrateFromInbox still refreshes DM call activity when inbox fails", async () => {
    const client = makeClient();
    let resolveCallHydration!: () => void;
    client.fetchInbox = mock(async () => {
      throw new Error("inbox unavailable");
    });
    client.hydrateRecentDmCallActivities = mock(() => new Promise<void>((resolve) => {
      resolveCallHydration = resolve;
    }));
    const { composable } = makeComposable({ client });

    const hydrated = await composable.hydrateFromInbox();

    expect(hydrated).toBe(false);
    expect(client.hydrateRecentDmCallActivities).toHaveBeenCalledTimes(1);
    resolveCallHydration();
  });

  test("hydrateFromInbox starts DM call activity before inbox returns", async () => {
    const client = makeClient();
    let resolveInbox!: (value: { totalUnread: number; conversations: InboxEntry[] }) => void;
    client.fetchInbox = mock(() => new Promise((resolve) => {
      resolveInbox = resolve;
    }));
    const { composable } = makeComposable({ client });

    const hydration = composable.hydrateFromInbox();

    expect(client.hydrateRecentDmCallActivities).toHaveBeenCalledTimes(1);

    resolveInbox({ totalUnread: 0, conversations: [] });
    expect(await hydration).toBe(true);
  });

  test("receiveIncomingDm creates conversation and increments unread", () => {
    const { composable } = makeComposable();
    composable.receiveIncomingDm(makeDmMessage());
    expect(composable.conversations.value).toHaveLength(1);
    expect(composable.conversations.value[0].unreadCount).toBe(1);
    expect(composable.conversations.value[0].lastMessageBody).toBe("hello");
    expect(composable.hasUnread.value).toBe(true);
    expect(composable.totalUnreadCount.value).toBe(1);
  });

  test("receiveIncomingDm files a MUC PM under the full occupant JID (#1256)", async () => {
    const { composable } = makeComposable();
    composable.receiveIncomingDm(makeDmMessage({
      mucPm: true,
      peerJid: "room@muc.example.com/juliet",
      fromJid: "room@muc.example.com/juliet",
      nick: "juliet",
      body: "occupant whisper",
    }));

    expect(composable.conversations.value).toHaveLength(1);
    // XEP-0045 §7.5: the conversation identity is the occupant JID —
    // keying by the room bare JID would make a reply broadcast.
    expect(composable.conversations.value[0]).toMatchObject({
      peerJid: "room@muc.example.com/juliet",
      // Room provenance in the display name keeps an occupant nick from
      // rendering byte-identical to a real account's DM entry.
      peerUsername: "juliet (room)",
      mucPm: true,
      mucPmRoomJid: "room@muc.example.com",
      lastMessageBody: "occupant whisper",
    });

    // Opening/marking the occupant conversation must not fold it back
    // into a room-bare-keyed sibling.
    await composable.openDm("room@muc.example.com/juliet");
    expect(composable.activePeerJid.value).toBe("room@muc.example.com/juliet");
    expect(composable.conversations.value).toHaveLength(1);
  });

  test("persist/restore round-trips occupant-keyed MUC-PM conversations", async () => {
    // #1256 P1 regression guard: restore() must NOT bare-fold an
    // occupant key back to the room bare JID — a reply from the folded
    // row would broadcast to the room.
    //
    // bun tests have no DOM: install a minimal window.sessionStorage so
    // the persist/restore pair actually executes.
    const store = new Map<string, string>();
    (globalThis as unknown as { window?: unknown }).window = {
      sessionStorage: {
        getItem: (key: string) => store.get(key) ?? null,
        setItem: (key: string, value: string) => void store.set(key, value),
        removeItem: (key: string) => void store.delete(key),
      },
    };
    const { composable, session } = makeComposable();
    composable.receiveIncomingDm(makeDmMessage({
      mucPm: true,
      peerJid: "room@muc.example.com/juliet",
      fromJid: "room@muc.example.com/juliet",
      nick: "juliet",
      body: "before reload",
    }));
    await composable.openDm("room@muc.example.com/juliet");
    await nextTick(); // flush the deep persist watcher

    // Simulate a reload: a fresh composable for the same session
    // restores the persisted blob via its immediate session watch.
    const { composable: reloaded } = makeComposable({ jid: session.value!.jid });
    await nextTick();

    expect(reloaded.conversations.value[0]).toMatchObject({
      peerJid: "room@muc.example.com/juliet",
      mucPm: true,
    });
    expect(reloaded.activePeerJid.value).toBe("room@muc.example.com/juliet");
    expect(reloaded.activeConversationScope.value).toBe("muc-occupant");
    delete (globalThis as unknown as { window?: unknown }).window;
  });

  test("restore keeps occupant keys even in blobs persisted before the mucPm flag", async () => {
    const store = new Map<string, string>();
    (globalThis as unknown as { window?: unknown }).window = {
      sessionStorage: {
        getItem: (key: string) => store.get(key) ?? null,
        setItem: (key: string, value: string) => void store.set(key, value),
        removeItem: (key: string) => void store.delete(key),
      },
    };
    // A blob written by an older build: occupant-keyed row without the
    // flag. Normal DM rows are always persisted bare, so the resource
    // heuristic must keep the key instead of folding it to the room.
    store.set("waddle.chat.dms.alice@example.com", JSON.stringify({
      conversations: [{
        peerJid: "room@muc.example.com/juliet",
        peerUsername: "juliet (room)",
        unreadCount: 0,
      }],
      activePeerJid: "room@muc.example.com/juliet",
    }));

    const { composable } = makeComposable();
    await nextTick();

    expect(composable.conversations.value[0]).toMatchObject({
      peerJid: "room@muc.example.com/juliet",
      mucPm: true,
    });
    expect(composable.activeConversationScope.value).toBe("muc-occupant");
    delete (globalThis as unknown as { window?: unknown }).window;
  });

  test("inbox entries for known MUC rooms are skipped (no phantom room-bare DM)", async () => {
    const client = makeClient([
      {
        partner: "room@muc.example.com",
        kind: "direct",
        lastStanzaId: "sid-pm",
        lastUpdated: epochSeconds("2026-01-02T12:00:00Z"),
        unread: 1,
        preview: "occupant whisper",
      },
      {
        partner: "bob@example.com",
        kind: "direct",
        lastStanzaId: "sid-1",
        lastUpdated: epochSeconds("2026-01-02T12:05:00Z"),
        unread: 0,
        preview: "hi",
      },
    ]);
    (client as unknown as { isKnownMucRoom: (jid: string) => boolean }).isKnownMucRoom =
      (jid: string) => jid === "room@muc.example.com";
    (client as unknown as { isMucPmPeer: (jid: string) => boolean }).isMucPmPeer = () => false;
    const { composable } = makeComposable({ client });

    // A phantom room-bare conversation left over from before the room
    // became known must be purged by the hydrate.
    composable.receiveIncomingDm(makeDmMessage({
      peerJid: "room@muc.example.com",
      fromJid: "room@muc.example.com",
      nick: "room",
      body: "old misfiled row",
    }));

    await composable.hydrateFromInbox();

    expect(composable.conversations.value.map((c) => c.peerJid)).toEqual(["bob@example.com"]);
  });

  test("restamp-only dispatches never touch conversations or unread", () => {
    const { composable } = makeComposable();
    composable.receiveIncomingDm(makeDmMessage({ body: "real copy" }));
    expect(composable.conversations.value[0].unreadCount).toBe(1);

    // The carbon restamp pass for the same stanza must be a no-op here.
    composable.receiveIncomingDm(makeDmMessage({
      body: "real copy",
      timestampRefreshOnly: true,
    }));
    expect(composable.conversations.value[0].unreadCount).toBe(1);
    expect(composable.conversations.value).toHaveLength(1);
  });

  test("receiveIncomingDm does not increment unread for active conversation", async () => {
    const { composable } = makeComposable();
    await composable.openDm("bob@example.com");
    composable.receiveIncomingDm(makeDmMessage());
    expect(composable.conversations.value[0].unreadCount).toBe(0);
    expect(composable.totalUnreadCount.value).toBe(0);
  });

  test("receiveIncomingDm does not auto-sync inbox read for active conversation", async () => {
    // Auto-mark-read responsibility lives in useChatReadReceipts (gated on
    // viewport + window focus); useDirectMessageConversations only suppresses the
    // unread increment for the active conversation.
    const client = makeClient();
    const { composable } = makeComposable({ client });

    await composable.openDm("bob@example.com");
    composable.receiveIncomingDm(makeDmMessage());

    expect(composable.conversations.value[0].unreadCount).toBe(0);
    expect(client.markInboxRead).not.toHaveBeenCalled();
  });

  test("receiveIncomingDm does not increment unread for self-sent messages", () => {
    const { composable } = makeComposable();
    composable.receiveIncomingDm(
      makeDmMessage({ fromJid: "alice@example.com", peerJid: "bob@example.com" }),
    );
    expect(composable.conversations.value[0].unreadCount).toBe(0);
  });

  test("receiveIncomingDm does not increment unread for archive-decoded catch-up re-emissions", () => {
    // MAM reconnect catch-up re-emits archive rows through the same
    // directMessage event; a message already counted live before the
    // reconnect must not be counted a second time. Genuinely-missed
    // messages are accounted by the server inbox hydrate instead.
    const { composable } = makeComposable();
    composable.receiveIncomingDm(makeDmMessage({ createdAt: "2026-01-02T00:00:00Z" }));
    expect(composable.conversations.value[0].unreadCount).toBe(1);
    composable.receiveIncomingDm(makeDmMessage({
      id: "msg-1-archive-copy",
      body: "hello again",
      createdAt: "2026-01-02T00:00:01Z",
      createdAtSource: "archive",
    }));
    expect(composable.conversations.value[0].unreadCount).toBe(1);
    // Preview/ordering still converge on the re-emitted content.
    expect(composable.conversations.value[0].lastMessageBody).toBe("hello again");
  });

  test("an archive re-emission of an older message does not roll the preview back", () => {
    const { composable } = makeComposable();
    composable.receiveIncomingDm(makeDmMessage({
      id: "msg-new",
      body: "newest live",
      createdAt: "2026-01-02T00:00:10Z",
    }));
    composable.receiveIncomingDm(makeDmMessage({
      id: "msg-old-archive-copy",
      body: "older archive copy",
      createdAt: "2026-01-02T00:00:00Z",
      createdAtSource: "archive",
    }));
    expect(composable.conversations.value[0].lastMessageBody).toBe("newest live");
    expect(composable.conversations.value[0].lastMessageAt).toBe("2026-01-02T00:00:10Z");
  });

  test("openDm does not auto-mark-read on its own", async () => {
    // Auto-mark-read responsibility lives in useChatReadReceipts (gated on
    // viewport + window focus); openDm only sets the active peer.
    const client = makeClient([
      {
        partner: "bob@example.com",
        kind: "direct",
        lastStanzaId: "sid-1",
        lastUpdated: epochSeconds("2026-01-02T12:00:00Z"),
        unread: 3,
        preview: "hydrate me",
      },
    ]);
    const { composable } = makeComposable({ client });

    await composable.hydrateFromInbox();
    await composable.openDm("bob@example.com");

    expect(composable.conversations.value[0].unreadCount).toBe(3);
    expect(composable.hasUnread.value).toBe(true);
    expect(composable.totalUnreadCount.value).toBe(3);
    expect(client.markInboxRead).not.toHaveBeenCalled();
  });

  test("markRead resets unread count", () => {
    const { composable } = makeComposable();
    composable.receiveIncomingDm(makeDmMessage());
    composable.receiveIncomingDm(makeDmMessage({ id: "msg-2" }));
    expect(composable.conversations.value[0].unreadCount).toBe(2);
    expect(composable.totalUnreadCount.value).toBe(2);
    composable.markRead("bob@example.com");
    expect(composable.conversations.value[0].unreadCount).toBe(0);
    expect(composable.hasUnread.value).toBe(false);
    expect(composable.totalUnreadCount.value).toBe(0);
  });

  test("onInboxPush applies direct unread totals and ignores non-DM entries", () => {
    const { composable } = makeComposable();

    composable.onInboxPush({
      partner: "bob@example.com",
      kind: "direct",
      lastStanzaId: "sid-1",
      lastUpdated: epochSeconds("2026-01-02T12:00:00Z"),
      unread: 4,
      preview: "from push",
    });
    composable.onInboxPush({
      partner: "general@conference.example.com",
      kind: "muc",
      lastStanzaId: "sid-2",
      lastUpdated: epochSeconds("2026-01-02T12:05:00Z"),
      unread: 3,
      preview: "ignore me",
    });

    expect(composable.conversations.value).toHaveLength(1);
    expect(composable.conversations.value[0]).toMatchObject({
      peerJid: "bob@example.com",
      lastMessageBody: "from push",
      unreadCount: 4,
    });
    expect(composable.totalUnreadCount.value).toBe(4);
  });

  test("does not double-count a DM already accounted by an inbox push", () => {
    const { composable } = makeComposable();

    composable.onInboxPush({
      partner: "bob@example.com",
      kind: "direct",
      lastStanzaId: "server-stanza-1",
      lastUpdated: epochSeconds("2026-01-02T12:00:00Z"),
      unread: 4,
      preview: "from push",
    });
    composable.receiveIncomingDm(makeDmMessage({
      id: "server-stanza-1",
      body: "same message",
      createdAt: "2026-01-02T12:00:01Z",
    }));

    expect(composable.conversations.value[0]).toMatchObject({
      lastMessageBody: "same message",
      unreadCount: 4,
    });
    expect(composable.totalUnreadCount.value).toBe(4);

    composable.onInboxPush({
      partner: "bob@example.com",
      kind: "direct",
      lastStanzaId: "server-stanza-2",
      lastUpdated: epochSeconds("2026-01-02T12:00:02Z"),
      unread: 5,
      preview: "from push again",
    });
    composable.receiveIncomingDm(makeDmMessage({
      id: "client-origin-2",
      wireIds: ["server-stanza-2"],
      body: "same wire id message",
      createdAt: "2026-01-02T12:00:02Z",
    }));

    expect(composable.conversations.value[0].unreadCount).toBe(5);

    composable.receiveIncomingDm(makeDmMessage({
      id: "server-stanza-3",
      body: "new message",
      createdAt: "2026-01-02T12:00:03Z",
    }));

    expect(composable.conversations.value[0].unreadCount).toBe(6);
  });

  test("updatePresence updates conversation presence", async () => {
    const { composable } = makeComposable();
    await composable.openDm("bob@example.com");
    composable.updatePresence({ bareJid: "bob@example.com", resource: "phone", show: "away" });
    expect(composable.conversations.value[0].presenceShow).toBe("away");
  });

  test("conversations sort by most recent message", () => {
    const { composable } = makeComposable();
    composable.receiveIncomingDm(
      makeDmMessage({ peerJid: "carol@example.com", fromJid: "carol@example.com", nick: "carol", createdAt: "2026-01-01T00:00:00Z" }),
    );
    composable.receiveIncomingDm(
      makeDmMessage({ peerJid: "bob@example.com", fromJid: "bob@example.com", nick: "bob", createdAt: "2026-01-02T00:00:00Z" }),
    );
    expect(composable.conversations.value[0].peerJid).toBe("bob@example.com");
    expect(composable.conversations.value[1].peerJid).toBe("carol@example.com");
  });
});
