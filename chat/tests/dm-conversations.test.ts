import { describe, test, expect, mock } from "bun:test";
import { ref } from "vue";
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
  hydrateRecentDmCallActivities: ReturnType<typeof mock>;
};

function makeClient(conversations: InboxEntry[] = []): MockClient {
  return {
    fetchInbox: mock(() => Promise.resolve({ totalUnread: conversations.reduce((sum, conversation) => sum + conversation.unread, 0), conversations })),
    markInboxRead: mock(() => Promise.resolve()),
    subscribeToPeerPresence: mock(() => Promise.resolve()),
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

  test("receiveIncomingDm creates conversation and increments unread", () => {
    const { composable } = makeComposable();
    composable.receiveIncomingDm(makeDmMessage());
    expect(composable.conversations.value).toHaveLength(1);
    expect(composable.conversations.value[0].unreadCount).toBe(1);
    expect(composable.conversations.value[0].lastMessageBody).toBe("hello");
    expect(composable.hasUnread.value).toBe(true);
    expect(composable.totalUnreadCount.value).toBe(1);
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
    composable.updatePresence({ bareJid: "bob@example.com", show: "away" });
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
