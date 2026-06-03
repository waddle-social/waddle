import { describe, expect, mock, test } from "bun:test";
import { computed, effectScope, ref } from "vue";
import { useChannelInbox } from "../src/channels/inbox";
import { useChatReadReceipts } from "../src/shell/read-receipts";
import { roomJidForChannelId } from "../src/lib/channel-room";
import type { ChannelSummary } from "../src/lib/chat-types";
import type { WaddleSession } from "../src/lib/server-auth";
import type { BrowserXmppClient, InboxEntry } from "../src/lib/xmpp-client";

type MockClient = BrowserXmppClient & {
  fetchInbox: ReturnType<typeof mock>;
  markInboxRead: ReturnType<typeof mock>;
};

function makeClient(conversations: InboxEntry[] = []): MockClient {
  return {
    fetchInbox: mock(() => Promise.resolve({
      totalUnread: conversations.reduce((sum, conversation) => sum + conversation.unread, 0),
      conversations,
    })),
    markInboxRead: mock(() => Promise.resolve()),
  } as unknown as MockClient;
}

const channelEntry: InboxEntry = {
  partner: "space_channel@conference.example.com",
  kind: "muc",
  lastStanzaId: "sid-1",
  lastUpdated: 100,
  unread: 2,
  preview: "channel preview",
};

const threadEntry: InboxEntry = {
  partner: "space_channel@conference.example.com",
  kind: "muc",
  lastStanzaId: "sid-2",
  lastUpdated: 200,
  unread: 3,
  thread: "thread-1",
  threadTitle: "Planning",
  preview: "thread preview",
};

describe("useChannelInbox", () => {
  test("hydrates channel and thread unread totals", async () => {
    const client = makeClient([
      channelEntry,
      threadEntry,
      {
        partner: "bob@example.com",
        kind: "direct",
        lastStanzaId: "sid-3",
        lastUpdated: 300,
        unread: 7,
      },
    ]);
    const composable = useChannelInbox(ref<BrowserXmppClient | null>(client));

    await composable.hydrateFromInbox();

    expect(composable.totalChannelUnreadCount.value).toBe(2);
    expect(composable.totalThreadUnreadCount.value).toBe(3);
    expect(composable.totalUnreadCount.value).toBe(2);
    expect(composable.channelUnreadMap([
      { id: "space_channel", jid: "space_channel@conference.example.com" },
    ])).toMatchObject({
      space_channel: {
        unread: 2,
        mentions: 0,
        threadUnread: 3,
        preview: "thread preview",
        lastUpdated: 200,
      },
    });
  });

  test("updates totals from inbox pushes and ignores direct entries", () => {
    const composable = useChannelInbox(ref<BrowserXmppClient | null>(null));

    composable.onInboxPush(channelEntry);
    composable.onInboxPush(threadEntry);
    composable.onInboxPush({
      partner: "bob@example.com",
      kind: "direct",
      lastStanzaId: "sid-3",
      lastUpdated: 300,
      unread: 7,
    });

    expect(composable.totalChannelUnreadCount.value).toBe(2);
    expect(composable.totalThreadUnreadCount.value).toBe(3);
    expect(composable.totalUnreadCount.value).toBe(2);
  });

  test("merges in-flight hydrate results with newer live inbox pushes", async () => {
    let resolveHydrate!: (value: { totalUnread: number; conversations: InboxEntry[] }) => void;
    const client = {
      fetchInbox: mock(() => new Promise<{ totalUnread: number; conversations: InboxEntry[] }>((resolve) => {
        resolveHydrate = resolve;
      })),
      markInboxRead: mock(() => Promise.resolve()),
    } as unknown as MockClient;
    const composable = useChannelInbox(ref<BrowserXmppClient | null>(client));

    const hydrated = composable.hydrateFromInbox();
    composable.onInboxPush({
      ...channelEntry,
      lastStanzaId: "sid-live",
      lastUpdated: 300,
      unread: 5,
      preview: "live unread",
    });

    resolveHydrate({
      totalUnread: 4,
      conversations: [{
        ...channelEntry,
        lastStanzaId: "sid-stale",
        lastUpdated: 100,
        unread: 0,
      }, {
        partner: "other@conference.example.com",
        kind: "muc",
        lastStanzaId: "sid-other",
        lastUpdated: 250,
        unread: 4,
        preview: "other unread",
      }],
    });

    expect(await hydrated).toBe(true);
    expect(composable.totalChannelUnreadCount.value).toBe(9);
    expect(composable.unreadForRoomJid("space_channel@conference.example.com")).toBe(5);
    expect(composable.unreadForRoomJid("other@conference.example.com")).toBe(4);
    expect(composable.channelUnreadMap([
      { id: "space_channel", jid: "space_channel@conference.example.com" },
      { id: "other", jid: "other@conference.example.com" },
    ])).toMatchObject({
      space_channel: {
        unread: 5,
        preview: "live unread",
        lastUpdated: 300,
      },
      other: {
        unread: 4,
        preview: "other unread",
        lastUpdated: 250,
      },
    });
  });

  test("ignores stale inbox pushes after a newer hydrate row", async () => {
    const client = makeClient([{
      ...channelEntry,
      lastStanzaId: "sid-newer",
      lastUpdated: 500,
      unread: 6,
      preview: "newer hydrate",
    }]);
    const composable = useChannelInbox(ref<BrowserXmppClient | null>(client));

    await composable.hydrateFromInbox();
    composable.onInboxPush({
      ...channelEntry,
      lastStanzaId: "sid-older",
      lastUpdated: 250,
      unread: 0,
      preview: "older push",
    });

    expect(composable.unreadForRoomJid("space_channel@conference.example.com")).toBe(6);
    expect(composable.channelUnreadMap([
      { id: "space_channel", jid: "space_channel@conference.example.com" },
    ])).toMatchObject({
      space_channel: {
        unread: 6,
        preview: "newer hydrate",
        lastUpdated: 500,
      },
    });
  });

  test("ignores stale inbox pushes after a newer live push", () => {
    const composable = useChannelInbox(ref<BrowserXmppClient | null>(null));

    composable.onInboxPush({
      ...channelEntry,
      lastStanzaId: "sid-newer-live",
      lastUpdated: 500,
      unread: 8,
      preview: "newer live",
    });
    composable.onInboxPush({
      ...channelEntry,
      lastStanzaId: "sid-older-live",
      lastUpdated: 300,
      unread: 0,
      preview: "older live",
    });

    expect(composable.unreadForRoomJid("space_channel@conference.example.com")).toBe(8);
    expect(composable.channelUnreadMap([
      { id: "space_channel", jid: "space_channel@conference.example.com" },
    ])).toMatchObject({
      space_channel: {
        unread: 8,
        preview: "newer live",
        lastUpdated: 500,
      },
    });
  });

  test("ignores equal-timestamp stale inbox pushes that would lower unread", () => {
    const composable = useChannelInbox(ref<BrowserXmppClient | null>(null));

    composable.onInboxPush({
      ...channelEntry,
      lastStanzaId: "sid-newer-same-second",
      lastUpdated: 500,
      unread: 5,
      preview: "newer same second",
    });
    composable.onInboxPush({
      ...channelEntry,
      lastStanzaId: "sid-older-same-second",
      lastUpdated: 500,
      unread: 0,
      preview: "older same second",
    });

    expect(composable.unreadForRoomJid("space_channel@conference.example.com")).toBe(5);
    expect(composable.channelUnreadMap([
      { id: "space_channel", jid: "space_channel@conference.example.com" },
    ])).toMatchObject({
      space_channel: {
        unread: 5,
        preview: "newer same second",
        lastUpdated: 500,
      },
    });
  });

  test("keeps a local mark-read across stale hydrate rows for the cleared stanza", async () => {
    const responses = [
      {
        totalUnread: 2,
        conversations: [channelEntry],
      },
      {
        totalUnread: 1,
        conversations: [{
          ...channelEntry,
          lastStanzaId: "sid-new",
          lastUpdated: 400,
          unread: 1,
          preview: "new unread",
        }],
      },
    ];
    const client = {
      fetchInbox: mock(() => Promise.resolve(responses.shift()!)),
      markInboxRead: mock(() => Promise.resolve()),
    } as unknown as MockClient;
    const composable = useChannelInbox(ref<BrowserXmppClient | null>(client));

    composable.onInboxPush(channelEntry);
    composable.markRead("space_channel@conference.example.com");
    expect(composable.unreadForRoomJid("space_channel@conference.example.com")).toBe(0);

    await composable.hydrateFromInbox();
    expect(composable.totalChannelUnreadCount.value).toBe(0);
    expect(composable.unreadForRoomJid("space_channel@conference.example.com")).toBe(0);

    await composable.hydrateFromInbox();
    expect(composable.unreadForRoomJid("space_channel@conference.example.com")).toBe(1);
  });

  test("does not replay channel mark-read onto newer hydrate rows", async () => {
    let resolveHydrate!: (value: { totalUnread: number; conversations: InboxEntry[] }) => void;
    const client = {
      fetchInbox: mock(() => new Promise<{ totalUnread: number; conversations: InboxEntry[] }>((resolve) => {
        resolveHydrate = resolve;
      })),
      markInboxRead: mock(() => Promise.resolve()),
    } as unknown as MockClient;
    const composable = useChannelInbox(ref<BrowserXmppClient | null>(client));

    composable.onInboxPush(channelEntry);
    const hydrated = composable.hydrateFromInbox();
    composable.markRead("space_channel@conference.example.com");

    resolveHydrate({
      totalUnread: 1,
      conversations: [{
        ...channelEntry,
        lastStanzaId: "sid-new",
        lastUpdated: 400,
        unread: 1,
        preview: "new unread",
      }],
    });

    expect(await hydrated).toBe(true);
    expect(composable.unreadForRoomJid("space_channel@conference.example.com")).toBe(1);
    expect(composable.totalChannelUnreadCount.value).toBe(1);
  });

  test("does not replay thread mark-read onto newer hydrate rows", async () => {
    let resolveHydrate!: (value: { totalUnread: number; conversations: InboxEntry[] }) => void;
    const client = {
      fetchInbox: mock(() => new Promise<{ totalUnread: number; conversations: InboxEntry[] }>((resolve) => {
        resolveHydrate = resolve;
      })),
      markInboxRead: mock(() => Promise.resolve()),
    } as unknown as MockClient;
    const composable = useChannelInbox(ref<BrowserXmppClient | null>(client));

    composable.onInboxPush(threadEntry);
    const hydrated = composable.hydrateFromInbox();
    composable.markThreadRead("space_channel@conference.example.com", "thread-1");

    resolveHydrate({
      totalUnread: 2,
      conversations: [{
        ...threadEntry,
        lastStanzaId: "sid-thread-new",
        lastUpdated: 450,
        unread: 2,
        preview: "new thread unread",
      }],
    });

    expect(await hydrated).toBe(true);
    expect(composable.totalThreadUnreadCount.value).toBe(2);
    expect(composable.threadEntries("space_channel@conference.example.com")[0]?.unread).toBe(2);
  });

  test("keeps thread mark-read barriers for thread ids containing separators", async () => {
    const specialThread = {
      ...threadEntry,
      thread: "parent::child",
      lastStanzaId: "sid-special-thread",
      unread: 4,
    };
    const responses = [
      {
        totalUnread: 4,
        conversations: [specialThread],
      },
    ];
    const client = {
      fetchInbox: mock(() => Promise.resolve(responses.shift()!)),
      markInboxRead: mock(() => Promise.resolve()),
    } as unknown as MockClient;
    const composable = useChannelInbox(ref<BrowserXmppClient | null>(client));

    composable.onInboxPush(specialThread);
    composable.markThreadRead("space_channel@conference.example.com", "parent::child");
    await composable.hydrateFromInbox();

    expect(composable.totalThreadUnreadCount.value).toBe(0);
    expect(composable.threadEntries("space_channel@conference.example.com")[0]?.threadId).toBe("parent::child");
    expect(composable.threadEntries("space_channel@conference.example.com")[0]?.unread).toBe(0);
    expect(client.markInboxRead).toHaveBeenCalledWith("space_channel@conference.example.com", "parent::child");
  });

  test("markRead and markThreadRead clear their own totals", () => {
    const client = makeClient();
    const composable = useChannelInbox(ref<BrowserXmppClient | null>(client));
    composable.onInboxPush(channelEntry);
    composable.onInboxPush(threadEntry);

    composable.markRead("space_channel@conference.example.com");

    expect(composable.totalChannelUnreadCount.value).toBe(0);
    expect(composable.totalThreadUnreadCount.value).toBe(3);
    expect(composable.totalUnreadCount.value).toBe(0);
    expect(client.markInboxRead).toHaveBeenCalledWith("space_channel@conference.example.com");

    composable.markThreadRead("space_channel@conference.example.com", "thread-1");

    expect(composable.totalThreadUnreadCount.value).toBe(0);
    expect(composable.totalUnreadCount.value).toBe(0);
    expect(client.markInboxRead).toHaveBeenCalledWith("space_channel@conference.example.com", "thread-1");
  });

  test("read receipts clear active discovered-MUC row and bell counts", async () => {
    const client = makeClient();
    const composable = useChannelInbox(ref<BrowserXmppClient | null>(client));
    const session = ref({
      jid: "alice@example.com/desktop",
      username: "alice",
      token: "token",
    } as WaddleSession);
    const activeChannelId = ref("c1");
    const channels = ref([
      { id: "c1", name: "general", jid: "c1@conference.example.net" },
    ] as ChannelSummary[]);
    const activeRoomJid = computed(() =>
      roomJidForChannelId(session.value, channels.value, activeChannelId.value),
    );
    const unreadCountForActive = computed(() =>
      composable.unreadForRoomJid(activeRoomJid.value),
    );
    const markDisplayed = mock(() => undefined);
    const scope = effectScope();

    composable.onInboxPush({
      partner: "c1@conference.example.net",
      kind: "muc",
      lastStanzaId: "sid-1",
      lastUpdated: 100,
      unread: 4,
    });

    expect(composable.totalUnreadCount.value).toBe(4);
    expect(unreadCountForActive.value).toBe(4);

    scope.run(() => {
      useChatReadReceipts({
        isWindowFocused: ref(true),
        isPinnedAtEdge: ref(true),
        activeKind: ref<"channel" | "dm" | null>("channel"),
        activeRoomJid,
        activePeerJid: ref(null),
        latestRemoteMessageId: ref("remote-1"),
        unreadCountForActive,
        markChannelRead: (jid) => composable.markRead(jid),
        markDmRead: () => undefined,
        markDisplayed,
      });
    });
    await Promise.resolve();

    expect(markDisplayed).toHaveBeenCalledWith("remote-1");
    expect(client.markInboxRead).toHaveBeenCalledWith("c1@conference.example.net");
    expect(composable.totalUnreadCount.value).toBe(0);
    expect(unreadCountForActive.value).toBe(0);

    scope.stop();
  });

  test("does not let a stale same-localpart inbox row mask the live room's unread", async () => {
    // Real-world scenario captured from user logs: the inbox holds rows
    // from a previously-used deployment (`muc.legacy.example`) alongside
    // the live deployment (`muc.waddle.social`). Both rooms share the
    // localpart `chat`. A localpart-keyed map silently overwrote the
    // live row, so opening the channel reported unread=0 and the
    // mark-read path never fired.
    const client = makeClient();
    const composable = useChannelInbox(ref<BrowserXmppClient | null>(client));
    composable.onInboxPush({
      partner: "chat@muc.waddle.social",
      kind: "muc",
      lastStanzaId: "live-1",
      lastUpdated: 200,
      unread: 38,
    });
    composable.onInboxPush({
      partner: "chat@muc.legacy.example",
      kind: "muc",
      lastStanzaId: "stale-1",
      lastUpdated: 100,
      unread: 0,
    });

    const liveChannel: Pick<ChannelSummary, "id" | "jid"> = {
      id: "chat",
      jid: "chat@muc.waddle.social",
    };

    expect(composable.unreadForRoomJid("chat@muc.waddle.social")).toBe(38);
    expect(composable.channelUnreadMap([liveChannel])).toMatchObject({
      chat: { unread: 38, mentions: 0 },
    });

    composable.markRead("chat@muc.waddle.social");

    expect(composable.unreadForRoomJid("chat@muc.waddle.social")).toBe(0);
    expect(composable.channelUnreadMap([liveChannel])).toMatchObject({
      chat: { unread: 0, mentions: 0 },
    });
    expect(composable.totalChannelUnreadCount.value).toBe(0);
    expect(client.markInboxRead).toHaveBeenCalledWith("chat@muc.waddle.social");
  });

  test("hydrate replaces stale local unread state with server-authoritative state", async () => {
    // Captures the "wrong after reconnect" / "unread counts stay stale"
    // symptom: device went offline (push stream interrupted), another
    // device marked-read while disconnected, the fresh reconnection
    // missed the headline push. On hydrate, the local state must be
    // overwritten by the server's view — not merged.
    const liveServerState: InboxEntry[] = [
      {
        partner: "space_channel@conference.example.com",
        kind: "muc",
        lastStanzaId: "sid-10",
        lastUpdated: 500,
        unread: 0,
      },
    ];
    const client = makeClient(liveServerState);
    const composable = useChannelInbox(ref<BrowserXmppClient | null>(client));

    // Local cache reflects the pre-disconnect state.
    composable.onInboxPush({
      partner: "space_channel@conference.example.com",
      kind: "muc",
      lastStanzaId: "sid-5",
      lastUpdated: 100,
      unread: 7,
      preview: "old preview",
    });
    expect(composable.totalChannelUnreadCount.value).toBe(7);

    // Fresh reconnection triggers hydrate — must replace the stale row.
    await composable.hydrateFromInbox();

    expect(composable.totalChannelUnreadCount.value).toBe(0);
    expect(composable.unreadForRoomJid("space_channel@conference.example.com")).toBe(0);
  });

  test("clears stale unread state without a client", async () => {
    const client = ref<BrowserXmppClient | null>(makeClient());
    const composable = useChannelInbox(client);
    composable.onInboxPush(channelEntry);
    composable.onInboxPush(threadEntry);

    client.value = null;
    const hydrated = await composable.hydrateFromInbox();

    expect(hydrated).toBe(false);
    expect(composable.totalChannelUnreadCount.value).toBe(0);
    expect(composable.totalThreadUnreadCount.value).toBe(0);
    expect(composable.totalUnreadCount.value).toBe(0);
  });
});
