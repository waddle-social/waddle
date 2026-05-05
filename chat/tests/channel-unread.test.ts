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
};

const threadEntry: InboxEntry = {
  partner: "space_channel@conference.example.com",
  kind: "muc",
  lastStanzaId: "sid-2",
  lastUpdated: 200,
  unread: 3,
  thread: "thread-1",
  threadTitle: "Planning",
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
      space_channel: { unread: 2, mentions: 0 },
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
    // from a previously-used deployment (`muc.waddle.social`) alongside
    // the live deployment (`muc.waddle.local`). Both rooms share the
    // localpart `chat`. A localpart-keyed map silently overwrote the
    // live row, so opening the channel reported unread=0 and the
    // mark-read path never fired.
    const client = makeClient();
    const composable = useChannelInbox(ref<BrowserXmppClient | null>(client));
    composable.onInboxPush({
      partner: "chat@muc.waddle.local",
      kind: "muc",
      lastStanzaId: "live-1",
      lastUpdated: 200,
      unread: 38,
    });
    composable.onInboxPush({
      partner: "chat@muc.waddle.social",
      kind: "muc",
      lastStanzaId: "stale-1",
      lastUpdated: 100,
      unread: 0,
    });

    const liveChannel: Pick<ChannelSummary, "id" | "jid"> = {
      id: "chat",
      jid: "chat@muc.waddle.local",
    };

    expect(composable.unreadForRoomJid("chat@muc.waddle.local")).toBe(38);
    expect(composable.channelUnreadMap([liveChannel])).toMatchObject({
      chat: { unread: 38, mentions: 0 },
    });

    composable.markRead("chat@muc.waddle.local");

    expect(composable.unreadForRoomJid("chat@muc.waddle.local")).toBe(0);
    expect(composable.channelUnreadMap([liveChannel])).toMatchObject({
      chat: { unread: 0, mentions: 0 },
    });
    expect(composable.totalChannelUnreadCount.value).toBe(0);
    expect(client.markInboxRead).toHaveBeenCalledWith("chat@muc.waddle.local");
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
